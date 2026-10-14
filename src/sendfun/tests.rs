use std::collections::HashMap;

use async_trait::async_trait;
use sendfun_sdk::constants::NEXUS_PROGRAM_ID;
use sendfun_sdk::dex::accounts::{POOL_DISCRIMINATOR, Pool};
use sendfun_sdk::dex::types::PoolStatus;
use sendfun_sdk::launchpad::accounts::{BONDING_CURVE_DISCRIMINATOR, BondingCurve};
use sendfun_sdk::launchpad::types::BondingCurveStatus;
use sendfun_sdk::math::amm::{MintFee, fee_on};
use sendfun_sdk::nexus::accounts::{PARTNER_CONFIG_DISCRIMINATOR, PartnerConfig};
use sendfun_sdk::nexus::pda::find_partner_config_pda;
use sendfun_sdk::nexus::types::{DexFees, LaunchpadFees};
use sendfun_sdk::utils::{Landing, TradeDirection};
use solana_account::Account;
use solana_address::Address;
use solana_program::clock::Clock;
use solana_program::sysvar::clock;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;

use super::bounds::{self, Limits, Quoter};
use super::convert::{to_address, to_pubkey};
use super::market::{MarketAccount, MarketKind, Pricing, Snapshot, TransferFee};
use super::venue::quote_snapshot;
use super::{DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, MIN_LOWER_BOUND, SendfunVenue};
use crate::account_caching::{AccountCacheError, AccountsCache};
use crate::trading_venue::error::{ErrorInfo, TradingVenueError};
use crate::trading_venue::{FromAccount, QuoteRequest, QuoteResult, SwapType, TradingVenue};

const NOW: i64 = 1_800_000_000;
const ONE_SOL: u64 = 1_000_000_000;

/// The quotes below do not read their mints.
const MINTS: (Pubkey, Pubkey) = (
    Pubkey::new_from_array([1; 32]),
    Pubkey::new_from_array([2; 32]),
);

/// 150 bps, plus a curve's creator fee rate, once the 20-second decay window
/// closes. The partner max differs from the creator fee rate of `CURVE`.
const LAUNCHPAD_FEES: LaunchpadFees = LaunchpadFees {
    creation_fee_cents: 0,
    protocol_fee_bps: 150,
    max_creator_fee_bps: 80,
    fee_decay_seconds: 20,
    fee_decay_start_bps: 5_000,
};

/// 75 bps, plus a pool's creator fee rate. The partner max differs from the
/// creator fee rate of `POOL`.
const DEX_FEES: DexFees = DexFees {
    creation_fee_cents: 0,
    protocol_fee_bps: 50,
    lp_fee_bps: 25,
    max_creator_fee_bps: 10,
    fee_decay_seconds: 0,
    fee_decay_start_bps: 0,
};

/// 300 bps, uncapped.
const UNCAPPED_300_BPS: MintFee = MintFee {
    bps: 300,
    maximum_fee: u64::MAX,
};

/// 300 bps, capped at 1,000,000 atoms.
const CAPPED_300_BPS: MintFee = MintFee {
    bps: 300,
    maximum_fee: 1_000_000,
};

/// A WSOL curve partway to completion. Its trades pay 200 bps.
const CURVE: Pricing = Pricing::Curve {
    virtual_base: 522_683_595_793_347,
    virtual_quote: 61_586_015_698,
    real_base: 242_783_595_793_347,
    real_quote: 31_586_015_698,
    creator_fee_bps: 50,
    fees: LAUNCHPAD_FEES,
};

/// A pool a curve migrated into. Its trades pay 100 bps.
const POOL: Pricing = Pricing::Pool {
    base_vault_amount: 145_102_644_403_718,
    quote_reserves: 121_208_052_469,
    creator_fee_bps: 25,
    fees: DEX_FEES,
};

fn snapshot(pricing: Pricing, base_fee: Option<MintFee>, quote_fee: Option<MintFee>) -> Snapshot {
    snapshot_of_age(pricing, 3_600, base_fee, quote_fee)
}

/// A snapshot at `NOW` of a market created `age` seconds before.
fn snapshot_of_age(
    pricing: Pricing,
    age: i64,
    base_fee: Option<MintFee>,
    quote_fee: Option<MintFee>,
) -> Snapshot {
    let landing = Landing {
        base_fee,
        quote_fee,
        unix_timestamp: NOW,
    };
    let created_at = NOW.checked_sub(age).unwrap();
    let fee_bps = pricing.fee_bps(created_at, NOW).unwrap();
    Snapshot {
        pricing,
        created_at,
        landing,
        fee_bps,
        base_fee: TransferFee::new(base_fee),
        quote_fee: TransferFee::new(quote_fee),
    }
}

fn quote(snapshot: &Snapshot, direction: TradeDirection, amount: u64) -> QuoteResult {
    quote_snapshot(snapshot, direction, amount, MINTS).unwrap()
}

/// The central difference of the SDK's output over `[x - h, x + h]`.
fn finite_difference(snapshot: &Snapshot, direction: TradeDirection, x: u64, h: u64) -> f64 {
    let out = |amount| quote(snapshot, direction, amount).expected_output as f64;
    let rise = out(x.checked_add(h).unwrap()) - out(x.checked_sub(h).unwrap());
    rise / h.checked_mul(2).unwrap() as f64
}

/// Asserts that the price at each `x` is within 0.1% of the central difference
/// over `x ± x/1000`.
fn assert_price_matches(snapshot: &Snapshot, direction: TradeDirection, amounts: &[u64]) {
    for &x in amounts {
        let price = quote(snapshot, direction, x).price;
        let difference = finite_difference(snapshot, direction, x, x / 1_000);
        let relative = (price - difference).abs() / difference;
        assert!(
            relative < 1e-3,
            "{direction:?} at {x}: price {price}, finite difference {difference}"
        );
    }
}

#[test]
fn the_program_ids_are_the_sdks() {
    use sendfun_sdk::constants;
    assert_eq!(
        LAUNCHPAD_PROGRAM_ID.to_bytes(),
        constants::LAUNCHPAD_PROGRAM_ID.to_bytes()
    );
    assert_eq!(
        DEX_PROGRAM_ID.to_bytes(),
        constants::DEX_PROGRAM_ID.to_bytes()
    );
}

#[test]
fn the_price_is_the_derivative_of_the_quote() {
    let buys = [10_000_000, 1_000_000_000, 20_000_000_000];
    let sells = [10_000_000_000, 1_000_000_000_000, 50_000_000_000_000];
    for (pricing, base_fee, quote_fee) in [
        (CURVE, None, None),
        (CURVE, None, Some(UNCAPPED_300_BPS)),
        (POOL, None, None),
        (POOL, Some(UNCAPPED_300_BPS), Some(UNCAPPED_300_BPS)),
    ] {
        let snapshot = snapshot(pricing, base_fee, quote_fee);
        assert_price_matches(&snapshot, TradeDirection::Buy, &buys);
        assert_price_matches(&snapshot, TradeDirection::Sell, &sells);
    }
}

#[test]
fn a_capped_fee_drops_out_of_the_price_past_its_cap() {
    // The cap is a transfer of 33_333_301 atoms, on either side.
    let snapshot = snapshot(POOL, Some(CAPPED_300_BPS), Some(CAPPED_300_BPS));
    assert_price_matches(
        &snapshot,
        TradeDirection::Buy,
        &[20_000_000, 50_000_000_000],
    );
    assert_price_matches(
        &snapshot,
        TradeDirection::Sell,
        &[20_000_000_000, 50_000_000_000_000],
    );
}

#[test]
fn the_spot_price_is_the_limit_of_the_price() {
    for snapshot in [
        snapshot(CURVE, None, None),
        snapshot(POOL, Some(UNCAPPED_300_BPS), Some(CAPPED_300_BPS)),
    ] {
        for direction in [TradeDirection::Buy, TradeDirection::Sell] {
            let spot = quote(&snapshot, direction, 0);
            assert_eq!((spot.amount, spot.expected_output), (0, 0));
            assert!(!spot.not_enough_liquidity);
            let near = quote(&snapshot, direction, MIN_LOWER_BOUND).price;
            assert!(spot.price > near && (spot.price - near) / spot.price < 1e-3);
        }
    }
}

#[test]
fn the_cap_point_is_the_first_transfer_paying_the_maximum() {
    let fee = TransferFee::new(Some(CAPPED_300_BPS));
    assert_eq!(fee.cap(), Some(33_333_301));
    assert_eq!(fee_on(33_333_300, Some(CAPPED_300_BPS)), Ok(999_999));
    assert_eq!(fee_on(33_333_301, Some(CAPPED_300_BPS)), Ok(1_000_000));
    assert_eq!(fee.received_at_cap(), Some(32_333_301));

    assert_eq!(TransferFee::new(Some(UNCAPPED_300_BPS)).cap(), None);
    assert_eq!(TransferFee::new(None).cap(), None);
    // A fee that is always 0 has no cap.
    for fee in [
        MintFee {
            bps: 300,
            maximum_fee: 0,
        },
        MintFee {
            bps: 0,
            maximum_fee: 1_000,
        },
    ] {
        assert_eq!(TransferFee::new(Some(fee)).cap(), None);
    }
}

/// The range of `direction` on `snapshot`, with each vault holding `vault`.
fn range(snapshot: &Snapshot, direction: TradeDirection, vault: u64) -> bounds::Range {
    let (input_fee, output_fee) = snapshot.transfer_fees(direction);
    let quoter = |amount: u64| quote_snapshot(snapshot, direction, amount, MINTS);
    bounds::range(&Limits {
        max_input: u64::MAX.checked_sub(vault).unwrap(),
        input_cap: input_fee.cap(),
        output_at_cap: output_fee.received_at_cap(),
        quoter: &quoter,
    })
}

#[test]
fn a_curve_buy_ends_below_the_supply_left() {
    let snapshot = snapshot(CURVE, None, None);
    let (lower, upper) = range(&snapshot, TradeDirection::Buy, 31_586_015_698).unwrap();
    assert_eq!((lower, upper), (1_000_000, 54_509_534_528));
    assert!(!quote(&snapshot, TradeDirection::Buy, upper).not_enough_liquidity);
    // Titan's search stops within 100 atoms of the edge.
    assert!(quote(&snapshot, TradeDirection::Buy, upper + 100).not_enough_liquidity);
}

#[test]
fn a_curve_sell_ends_below_the_quote_it_holds() {
    let snapshot = snapshot(CURVE, None, None);
    let (lower, upper) = range(&snapshot, TradeDirection::Sell, 449_683_595_793_347).unwrap();
    assert_eq!((lower, upper), (1_000_000, 550_316_408_762_944));
    assert!(quote_snapshot(&snapshot, TradeDirection::Sell, upper + 100, MINTS).is_err());
}

#[test]
fn a_pool_buy_ends_where_the_input_vault_fills() {
    let snapshot = snapshot(POOL, None, None);
    let vault = 121_229_685_698;
    let (lower, upper) = range(&snapshot, TradeDirection::Buy, vault).unwrap();
    assert_eq!(lower, 1_000_000);
    assert!(upper <= u64::MAX - vault && u64::MAX - vault - upper <= 100);
}

/// 300 bps, `ceil(3x / 100)`, reaches its 1,000,000 maximum at a transfer of
/// 33,333,301, which lands 32,333,301. The fee is flat past that point, and
/// the pool runs to `u64::MAX`. Each direction routes that side.
#[test]
fn a_capped_fee_routes_the_wider_side_of_its_cap_point() {
    let snapshot = snapshot(POOL, None, Some(CAPPED_300_BPS));
    let buy = range(&snapshot, TradeDirection::Buy, 0).unwrap();
    assert_eq!(buy, (33_333_301, 18_446_744_073_709_551_550));
    let sell = range(&snapshot, TradeDirection::Sell, 0).unwrap();
    assert_eq!(sell, (40_318_806_443, 18_446_744_073_709_551_550));
    let received = quote(&snapshot, TradeDirection::Sell, sell.0).expected_output;
    assert_eq!(received, 32_333_302);
}

#[test]
fn the_certified_range_passes_titans_checks() {
    for (pricing, base_fee, quote_fee) in [
        (CURVE, None, None),
        (CURVE, None, Some(UNCAPPED_300_BPS)),
        (POOL, None, None),
        (POOL, Some(UNCAPPED_300_BPS), Some(CAPPED_300_BPS)),
    ] {
        let snapshot = snapshot(pricing, base_fee, quote_fee);
        for direction in [TradeDirection::Buy, TradeDirection::Sell] {
            let (lower, upper) = range(&snapshot, direction, 0).unwrap();
            let quoter = |amount: u64| quote_snapshot(&snapshot, direction, amount, MINTS);
            assert!(bounds::passes(&quoter as &dyn Quoter, lower, upper));
        }
    }
}

const MARKET: Pubkey = Pubkey::new_from_array([3; 32]);
const BASE_MINT: Pubkey = Pubkey::new_from_array([4; 32]);
const QUOTE_MINT: Pubkey = Pubkey::new_from_array([5; 32]);
const BASE_VAULT: Pubkey = Pubkey::new_from_array([6; 32]);
const QUOTE_VAULT: Pubkey = Pubkey::new_from_array([7; 32]);
const PLATFORM: Pubkey = Pubkey::new_from_array([8; 32]);
/// A partner registered on `PLATFORM`.
const PARTNER: Pubkey = Pubkey::new_from_array([12; 32]);

/// The launchpad schedule of the SDK's quote tests: 100 bps, plus a curve's
/// creator fee rate, plus a premium that decays from 5,000 bps over 12
/// seconds.
const fn sdk_launchpad_fees(max_creator_fee_bps: u16) -> LaunchpadFees {
    LaunchpadFees {
        creation_fee_cents: 0,
        protocol_fee_bps: 100,
        max_creator_fee_bps,
        fee_decay_seconds: 12,
        fee_decay_start_bps: 5_000,
    }
}

/// The DEX schedule of the SDK's quote tests: 110 bps, plus a pool's creator
/// fee rate, plus a premium that decays from 5,000 bps over 12 seconds.
const fn sdk_dex_fees(max_creator_fee_bps: u16) -> DexFees {
    DexFees {
        creation_fee_cents: 0,
        protocol_fee_bps: 80,
        lp_fee_bps: 30,
        max_creator_fee_bps,
        fee_decay_seconds: 12,
        fee_decay_start_bps: 5_000,
    }
}

/// The curve of the SDK's quote tests.
const fn sdk_curve(creator_fee_bps: u16, max_creator_fee_bps: u16) -> Pricing {
    Pricing::Curve {
        virtual_base: 1_000_000_000_000_000,
        virtual_quote: 30_000_000_000,
        real_base: 500_000_000_000_000,
        real_quote: 0,
        creator_fee_bps,
        fees: sdk_launchpad_fees(max_creator_fee_bps),
    }
}

/// The pool of the SDK's quote tests.
const fn sdk_pool(creator_fee_bps: u16, max_creator_fee_bps: u16) -> Pricing {
    Pricing::Pool {
        base_vault_amount: 400_000_000_000,
        quote_reserves: 25_000_000_000,
        creator_fee_bps,
        fees: sdk_dex_fees(max_creator_fee_bps),
    }
}

/// `(fee_bps, fee, out_amount)` of a buy of 1 SOL on `pricing`, `age` seconds
/// after the market's creation.
fn buy_one_sol(pricing: Pricing, age: i64) -> (u16, u64, u64) {
    let snapshot = snapshot_of_age(pricing, age, None, None);
    let bought = snapshot.market_quote(TradeDirection::Buy, ONE_SOL).unwrap();
    // The price charges the rate that the quote charges.
    assert_eq!(snapshot.fee_bps, bought.fee_bps);
    assert_price_matches(&snapshot, TradeDirection::Buy, &[ONE_SOL]);
    (bought.fee_bps, bought.fee, bought.out_amount)
}

/// A curve's rate can be above the partner max. A partner can lower its max
/// after the curve's creation. The SDK's quote tests assert the same values.
#[test]
fn the_curves_creator_rate_prices_the_trade_not_the_partner_max() {
    // `(fee_bps, fee, out_amount)` past the decay window, then halfway through
    // it.
    for (creator_fee_bps, standard, halfway) in [
        (
            0,
            (100, 10_000_000, 31_945_788_964_181),
            (1_325, 132_500_000, 28_103_992_872_762),
        ),
        (
            30,
            (130, 13_000_000, 31_852_066_995_836),
            (1_348, 134_800_000, 28_031_569_534_621),
        ),
        (
            80,
            (180, 18_000_000, 31_695_823_381_318),
            (1_385, 138_500_000, 27_915_039_774_476),
        ),
    ] {
        for max_creator_fee_bps in [0, 50, u16::MAX] {
            let curve = sdk_curve(creator_fee_bps, max_creator_fee_bps);
            assert_eq!(buy_one_sol(curve.clone(), 100), standard);
            assert_eq!(buy_one_sol(curve, 6), halfway);
        }
    }
}

/// A pool's rate can be above the partner max. A partner can lower its max
/// after the pool's creation. The SDK's quote tests assert the same values.
#[test]
fn the_pools_creator_rate_prices_the_trade_not_the_partner_max() {
    // `(fee_bps, fee, out_amount)` past the decay window, then halfway through
    // it.
    for (creator_fee_bps, standard, halfway) in [
        (
            0,
            (110, 11_000_000, 15_221_824_618),
            (1_333, 133_300_000, 13_402_560_048),
        ),
        (
            40,
            (150, 15_000_000, 15_162_593_804),
            (1_363, 136_300_000, 13_357_717_573),
        ),
        (
            160,
            (270, 27_000_000, 14_984_791_899),
            (1_453, 145_300_000, 13_223_127_709),
        ),
    ] {
        for max_creator_fee_bps in [0, 100, u16::MAX] {
            let pool = sdk_pool(creator_fee_bps, max_creator_fee_bps);
            assert_eq!(buy_one_sol(pool.clone(), 100), standard);
            assert_eq!(buy_one_sol(pool, 6), halfway);
        }
    }
}

fn account(owner: Pubkey, data: Vec<u8>) -> Account {
    Account {
        lamports: 1_000_000_000,
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}

/// A `Funding` curve with the reserves of `sdk_curve`, created 100 seconds
/// before `NOW`.
fn curve() -> BondingCurve {
    BondingCurve {
        discriminator: BONDING_CURVE_DISCRIMINATOR,
        version: 1,
        bump: 255,
        status: BondingCurveStatus::Funding,
        base_mint: to_address(&BASE_MINT),
        base_decimals: 6,
        base_supply: 1_000_000_000_000_000,
        initial_virtual_base: 1_000_000_000_000_000,
        initial_virtual_quote: 30_000_000_000,
        initial_real_base: 500_000_000_000_000,
        quote_mint: to_address(&QUOTE_MINT),
        quote_decimals: 9,
        coin_creator: Address::new_from_array([9; 32]),
        padding0: [0; 4],
        partner: to_address(&PARTNER),
        base_vault: to_address(&BASE_VAULT),
        quote_vault: to_address(&QUOTE_VAULT),
        virtual_base_reserves: 1_000_000_000_000_000,
        virtual_quote_reserves: 30_000_000_000,
        real_base_reserves: 500_000_000_000_000,
        real_quote_reserves: 0,
        created_at: NOW - 100,
        protocol_owed: 0,
        creator_owed: 0,
        platform_config: to_address(&PLATFORM),
        creator_fee_mode: 0,
        creator_fee_bps: 0,
        dex_creator_fee_bps: 0,
        reserved: [0; 256],
    }
}

/// An `Active` pool with the reserves of `sdk_pool`, created 100 seconds
/// before `NOW`.
fn pool() -> Pool {
    Pool {
        discriminator: POOL_DISCRIMINATOR,
        version: 1,
        bump: 255,
        status: PoolStatus::Active,
        base_mint: to_address(&BASE_MINT),
        base_decimals: 6,
        quote_mint: to_address(&QUOTE_MINT),
        quote_decimals: 9,
        coin_creator: Address::new_from_array([9; 32]),
        padding0: [0; 4],
        base_vault: to_address(&BASE_VAULT),
        quote_vault: to_address(&QUOTE_VAULT),
        lp_mint: Address::new_from_array([10; 32]),
        lp_supply: 100_000_000_000,
        base_reserves: 400_000_000_000,
        quote_reserves: 25_000_000_000,
        quote_ledger: 25_000_000_000,
        created_at: NOW - 100,
        protocol_owed: 0,
        creator_owed: 0,
        platform_config: to_address(&PLATFORM),
        creator_fee_mode: 0,
        creator_fee_bps: 0,
        reserved: [0; 256],
    }
}

/// `PARTNER`'s config on `PLATFORM`. Its creator fee max is 50 bps
/// on the launchpad and 100 bps on the DEX.
fn partner_config() -> PartnerConfig {
    PartnerConfig {
        discriminator: PARTNER_CONFIG_DISCRIMINATOR,
        version: 1,
        bump: 255,
        authority: Address::new_from_array([11; 32]),
        partner: to_address(&PARTNER),
        fee_preset: 0,
        launchpad: sdk_launchpad_fees(50),
        dex: sdk_dex_fees(100),
        platform_config: to_address(&PLATFORM),
        allowed_creator_fee_modes: 0,
        reserved: [0; 256],
    }
}

fn curve_account(owner: Pubkey, discriminator: [u8; 8]) -> Account {
    let curve = BondingCurve {
        discriminator,
        ..curve()
    };
    account(owner, borsh::to_vec(&curve).unwrap())
}

#[test]
fn a_market_account_gives_pricing_its_own_creator_rate() {
    let partner = partner_config();

    let curve = BondingCurve {
        creator_fee_bps: 30,
        dex_creator_fee_bps: 70,
        ..curve()
    };
    let market =
        MarketAccount::decode(MarketKind::Launchpad, &borsh::to_vec(&curve).unwrap()).unwrap();
    assert_eq!(market.creator_fee_bps, 30);
    assert_eq!(Pricing::new(&market, &partner, 0), sdk_curve(30, 50));

    let pool = Pool {
        creator_fee_bps: 40,
        ..pool()
    };
    let market = MarketAccount::decode(MarketKind::Dex, &borsh::to_vec(&pool).unwrap()).unwrap();
    assert_eq!(market.creator_fee_bps, 40);
    assert_eq!(
        Pricing::new(&market, &partner, 400_000_000_000),
        sdk_pool(40, 100)
    );
}

/// The accounts that an update serves.
struct Accounts(HashMap<Pubkey, Account>);

#[async_trait]
impl AccountsCache for Accounts {
    async fn get_account(&self, pubkey: &Pubkey) -> Result<Option<Account>, AccountCacheError> {
        Ok(self.0.get(pubkey).cloned())
    }

    async fn get_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<Account>>, AccountCacheError> {
        Ok(pubkeys.iter().map(|key| self.0.get(key).cloned()).collect())
    }
}

/// What an update of `MARKET` reads: `market`, two SPL Token mints, the
/// vaults, `PARTNER`'s config and the clock at `NOW`.
fn accounts(market: Account, base_vault_amount: u64) -> Accounts {
    let mint = |decimals: u8| {
        let mint = spl_token::state::Mint {
            decimals,
            is_initialized: true,
            ..spl_token::state::Mint::default()
        };
        let mut data = vec![0; spl_token::state::Mint::LEN];
        spl_token::state::Mint::pack(mint, &mut data).unwrap();
        account(spl_token::ID, data)
    };
    let vault = |mint: Pubkey, amount: u64| {
        let vault = spl_token::state::Account {
            mint,
            owner: MARKET,
            amount,
            state: spl_token::state::AccountState::Initialized,
            ..spl_token::state::Account::default()
        };
        let mut data = vec![0; spl_token::state::Account::LEN];
        spl_token::state::Account::pack(vault, &mut data).unwrap();
        account(spl_token::ID, data)
    };
    let partner_config_key =
        find_partner_config_pda(&to_address(&PLATFORM), &to_address(&PARTNER)).0;
    let clock = Clock {
        unix_timestamp: NOW,
        ..Clock::default()
    };
    Accounts(HashMap::from([
        (MARKET, market),
        (BASE_MINT, mint(6)),
        (QUOTE_MINT, mint(9)),
        (BASE_VAULT, vault(BASE_MINT, base_vault_amount)),
        (QUOTE_VAULT, vault(QUOTE_MINT, 0)),
        (
            to_pubkey(&partner_config_key),
            account(
                to_pubkey(&NEXUS_PROGRAM_ID),
                borsh::to_vec(&partner_config()).unwrap(),
            ),
        ),
        (
            clock::ID,
            account(
                solana_program::sysvar::ID,
                bincode::serialize(&clock).unwrap(),
            ),
        ),
    ]))
}

/// The venue's quote of a buy of 1 SOL on `market`, after one update.
async fn updated_buy(market: Account, base_vault_amount: u64) -> QuoteResult {
    let accounts = accounts(market, base_vault_amount);
    let mut venue =
        SendfunVenue::from_account_with_partner(&MARKET, &accounts.0[&MARKET], &PARTNER).unwrap();
    venue.update_state(&accounts).await.unwrap();
    assert_eq!(venue.halted(), None);
    venue
        .quote(QuoteRequest {
            input_mint: QUOTE_MINT,
            output_mint: BASE_MINT,
            amount: ONE_SOL,
            swap_type: SwapType::ExactIn,
        })
        .unwrap()
}

/// Asserts that `bought` carries, bit for bit, the price of a buy of 1 SOL on
/// `pricing` 100 seconds after the market's creation. `buy_one_sol` checks
/// that price against the quote.
fn assert_same_price(bought: &QuoteResult, pricing: Pricing) {
    let snapshot = snapshot_of_age(pricing, 100, None, None);
    let priced = quote(&snapshot, TradeDirection::Buy, ONE_SOL);
    assert_eq!(bought.price.to_bits(), priced.price.to_bits());
}

/// The partner config's max is 50 bps on the launchpad and 100 bps on the
/// DEX. `creator_fee_mode` sets who claims the creator fee, not the rate.
#[tokio::test]
async fn an_update_reads_the_creator_rate_from_the_market_account() {
    for creator_fee_mode in [0, 1] {
        for (creator_fee_bps, expected_output) in [
            (0, 31_945_788_964_181),
            (30, 31_852_066_995_836),
            (80, 31_695_823_381_318),
        ] {
            let curve = BondingCurve {
                creator_fee_mode,
                creator_fee_bps,
                dex_creator_fee_bps: 70,
                ..curve()
            };
            let market = account(LAUNCHPAD_PROGRAM_ID, borsh::to_vec(&curve).unwrap());
            let bought = updated_buy(market, 500_000_000_000_000).await;
            assert_eq!(
                (bought.amount, bought.expected_output),
                (ONE_SOL, expected_output)
            );
            assert_same_price(&bought, sdk_curve(creator_fee_bps, 50));
        }
        for (creator_fee_bps, expected_output) in [
            (0, 15_221_824_618),
            (40, 15_162_593_804),
            (160, 14_984_791_899),
        ] {
            let pool = Pool {
                creator_fee_mode,
                creator_fee_bps,
                ..pool()
            };
            let market = account(DEX_PROGRAM_ID, borsh::to_vec(&pool).unwrap());
            let bought = updated_buy(market, 400_000_000_000).await;
            assert_eq!(
                (bought.amount, bought.expected_output),
                (ONE_SOL, expected_output)
            );
            assert_same_price(&bought, sdk_pool(creator_fee_bps, 100));
        }
    }
}

#[test]
fn a_venue_builds_from_a_curve_and_waits_for_an_update() {
    let account = curve_account(LAUNCHPAD_PROGRAM_ID, BONDING_CURVE_DISCRIMINATOR);
    let venue =
        SendfunVenue::from_account_with_partner(&Pubkey::new_unique(), &account, &PARTNER).unwrap();
    assert_eq!(venue.partner(), PARTNER);
    assert!(!venue.initialized());
    assert_eq!(venue.halted(), Some("market not updated"));
    assert_eq!(venue.program_id(), LAUNCHPAD_PROGRAM_ID);
    assert_eq!(String::from(venue.protocol()), "send.fun Launchpad");
    assert!(venue.get_token_info().is_empty());
}

#[test]
fn from_account_refuses_other_accounts_without_allocating() {
    let wrong_owner = curve_account(Pubkey::new_unique(), BONDING_CURVE_DISCRIMINATOR);
    let wrong_discriminator = curve_account(LAUNCHPAD_PROGRAM_ID, [0; 8]);
    let pool_owned_curve = curve_account(DEX_PROGRAM_ID, BONDING_CURVE_DISCRIMINATOR);
    for (account, reason) in [
        (
            wrong_owner,
            "account is not owned by the send.fun launchpad or DEX",
        ),
        (
            wrong_discriminator,
            "account is not a send.fun bonding curve",
        ),
        (pool_owned_curve, "account is not a send.fun DEX pool"),
    ] {
        let Err(TradingVenueError::FromAccountError(ErrorInfo::StaticStr(error))) =
            SendfunVenue::from_account_with_partner(&Pubkey::new_unique(), &account, &PARTNER)
        else {
            panic!("expected a static FromAccountError");
        };
        assert_eq!(error, reason);
    }
}

/// No partner is assumed. No test of this crate sets one, so `from_account`
/// refuses a valid market.
#[test]
fn from_account_refuses_without_a_partner() {
    assert_eq!(super::partner(), None);
    let account = curve_account(LAUNCHPAD_PROGRAM_ID, BONDING_CURVE_DISCRIMINATOR);
    let Err(TradingVenueError::FromAccountError(ErrorInfo::StaticStr(error))) =
        SendfunVenue::from_account(&Pubkey::new_unique(), &account)
    else {
        panic!("expected a static FromAccountError");
    };
    assert_eq!(
        error,
        "no send.fun partner is set: call sendfun::set_partner first"
    );
}
