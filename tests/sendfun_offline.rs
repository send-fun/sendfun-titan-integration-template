//! The send.fun venue on mainnet market snapshots. Needs no RPC and no program
//! binaries.
//!
//! `tests/fixtures/accounts/<market>/` holds each market's accounts, one
//! bincode `Account` per `<address>.bin`, and the clock at the read. A test
//! can change one thing in a snapshot: the clock, a mint's pause or default
//! state, the curve's status, a creator fee rate, or a hook on the quote mint.
//!
//! The tests cover Titan's pricing checks, bounds, updates, refusals and the
//! swap instruction's accounts. The quote math is `sendfun-sdk`'s.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use assert_no_alloc::assert_no_alloc;
use async_trait::async_trait;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::{Pubkey, pubkey};
use solana_sysvar::clock::{self, Clock};
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_discriminator::{ArrayDiscriminator, SplDiscriminate};
use spl_tlv_account_resolution::account::ExtraAccountMeta;
use spl_tlv_account_resolution::pubkey_data::PubkeyData;
use spl_tlv_account_resolution::seeds::Seed;
use spl_tlv_account_resolution::state::ExtraAccountMetaList;

use sendfun_sdk::dex::accounts::Pool;
use sendfun_sdk::launchpad::accounts::BondingCurve;
use sendfun_sdk::launchpad::create::{BuildCreateTokenParams, build_create_token_instruction};
use sendfun_sdk::nexus::accounts::PartnerConfig;
use titan_integration_template::account_caching::{AccountCacheError, AccountsCache};
use titan_integration_template::sendfun::{
    DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, SendfunVenue, parse_pool_creations, set_partner,
};
use titan_integration_template::swap_route::{ROUTE_WEIGHT_ALL, Venue, build_swap_leg};
use titan_integration_template::trading_venue::error::{ErrorInfo, TradingVenueError};
use titan_integration_template::trading_venue::protocol::PoolProtocol;
use titan_integration_template::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};
use titan_integration_template::trading_venue::{
    FromAccount, QuoteRequest, SwapType, TradingVenue,
};

// The allocation guard behind `assert_no_alloc`. In a build with debug
// assertions, it aborts on an allocation inside a guarded closure.
#[cfg(debug_assertions)]
#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// A launchpad bonding curve quoted in WSOL.
const CURVE_WSOL: Pubkey = pubkey!("Eq52yham9BNMbz2pCcqpSFUNAfJZKJFPNEhqHaEdyy7L");
const CURVE_WSOL_BASE: Pubkey = pubkey!("Sg18DfCS5Ne8JujHcCsidTctSacmAhRXKM6BCG4send");
/// A launchpad bonding curve quoted in PRELWG, a Token-2022 mint with a
/// transfer fee.
const CURVE_FEE: Pubkey = pubkey!("ECxRrNU6Ggd73c3AD7UPd6ijfJYdULWFT2jDJZu5gzcd");
const PRELWG: Pubkey = pubkey!("PreLWGkkeqG1s4HEfFZSy9moCrJ7btsHuUtfcCeoRua");
const CURVE_FEE_BASE: Pubkey = pubkey!("8kNhVKSuv5fEXiiSXGfVKd264dbqEP1veN6fZmXcsend");
/// A launchpad bonding curve quoted in an SPL Token mint.
const CURVE_SPL: Pubkey = pubkey!("HafB4R3M8KMmLZxb44qmL3TkxxWNCNMqXr4Zmg2oJMVo");
/// A DEX pool quoted in WSOL. A completed curve migrated into it.
const POOL_WSOL: Pubkey = pubkey!("ChJu4A7Gow1DDsWdgHvcpJtxDQQZvA5tavWKn8zZ9kY5");
const POOL_WSOL_BASE: Pubkey = pubkey!("eRL5z1LmGvJxXj8MxFZSNXsukVgF23egb6ZxXnRsend");

const WSOL: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN_2022_PROGRAM: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA_PROGRAM: Pubkey = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
const SYSTEM_PROGRAM: Pubkey = pubkey!("11111111111111111111111111111111");
const RENT_SYSVAR: Pubkey = pubkey!("SysvarRent111111111111111111111111111111111");
/// The platform of every snapshot market.
const PLATFORM: Pubkey = pubkey!("2PJedAsa7pCnScks2XM3U4o2nPaTvvBmi2553VcCnGWB");
/// The partner the tests route as. `Fixtures::load` registers it on `PLATFORM`.
const PARTNER: Pubkey = Pubkey::new_from_array([12; 32]);
/// The config that each snapshot holds: the default partner's, the System
/// Program key, which signs nothing. `Fixtures::load` moves it to `PARTNER`.
const SNAPSHOT_PARTNER_CONFIG: Pubkey = pubkey!("B77P3Hvj9xLi4tcFtyjNcf6NjhkQrfJvLkz3nVZnBzec");
const NEXUS_PROGRAM: Pubkey = pubkey!("7rrCurqdWFesbzwtYPo95fM8waTtVXgXwCom45x1sfNX");
/// A transfer-hook program's address. Nothing runs it.
const HOOK_PROGRAM: Pubkey = pubkey!("8mTbJtVKaLQ9pVgEMkjgu2N3EvBKyMXBxkPZvBW8d5Qk");

/// The four snapshot markets.
const MARKETS: [Pubkey; 4] = [CURVE_WSOL, CURVE_FEE, CURVE_SPL, POOL_WSOL];

/// The status byte's offset in a curve or pool account.
const STATUS_OFFSET: usize = 10;
/// Token-2022 extension types.
const DEFAULT_ACCOUNT_STATE_EXTENSION: u16 = 6;
const TRANSFER_FEE_EXTENSION: u16 = 1;
const TRANSFER_HOOK_EXTENSION: u16 = 14;
const PAUSABLE_EXTENSION: u16 = 26;
/// The offset of a Token-2022 mint's TLV region, after its account type byte.
const TLV_START: usize = 166;

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// One market's snapshot, keyed by address.
#[derive(Clone)]
struct Fixtures(BTreeMap<Pubkey, Account>);

#[async_trait]
impl AccountsCache for Fixtures {
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

impl Fixtures {
    /// The snapshot as read from mainnet. Its only partner config is the
    /// default partner's: the System Program key's.
    fn snapshot(market: &Pubkey) -> Self {
        let dir = Path::new("tests/fixtures/accounts").join(market.to_string());
        let mut accounts = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let key: Pubkey = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
            let account: Account = bincode::deserialize_from(File::open(&path).unwrap()).unwrap();
            accounts.insert(key, account);
        }
        Self(accounts)
    }

    /// The snapshot with `PARTNER` registered in place of the default
    /// partner, with the same schedule.
    fn load(market: &Pubkey) -> Self {
        let Self(mut accounts) = Self::snapshot(market);
        let mut registered = accounts.remove(&SNAPSHOT_PARTNER_CONFIG).unwrap();
        let mut config = PartnerConfig::from_bytes(&registered.data).unwrap();
        config.partner = solana_address::Address::new_from_array(PARTNER.to_bytes());
        registered.data = borsh::to_vec(&config).unwrap();
        accounts.insert(partner_config(&PLATFORM), registered);
        Self(accounts)
    }

    fn account(&self, key: &Pubkey) -> &Account {
        &self.0[key]
    }

    fn account_mut(&mut self, key: &Pubkey) -> &mut Account {
        self.0.get_mut(key).unwrap()
    }

    fn clock(&self) -> Clock {
        bincode::deserialize(&self.account(&clock::ID).data).unwrap()
    }

    fn set_unix_timestamp(&mut self, unix_timestamp: i64) {
        let clock = Clock {
            unix_timestamp,
            ..self.clock()
        };
        self.account_mut(&clock::ID).data = bincode::serialize(&clock).unwrap();
    }

    /// Sets `curve`'s `created_at`, as a relaunch does.
    fn relaunch(&mut self, curve: &Pubkey, created_at: i64) {
        let mut account = BondingCurve::from_bytes(&self.account(curve).data).unwrap();
        account.created_at = created_at;
        self.account_mut(curve).data = borsh::to_vec(&account).unwrap();
    }

    /// Returns `curve` to its reserves at creation, before any buy.
    fn undo_buys(&mut self, curve: &Pubkey) {
        let mut account = BondingCurve::from_bytes(&self.account(curve).data).unwrap();
        account.virtual_base_reserves = account.initial_virtual_base;
        account.virtual_quote_reserves = account.initial_virtual_quote;
        account.real_base_reserves = account.initial_real_base;
        account.real_quote_reserves = 0;
        self.account_mut(curve).data = borsh::to_vec(&account).unwrap();
    }

    /// Moves `curve` to a new platform, where `partner` has the same config.
    /// Returns the platform. The config on the old platform stays.
    fn move_platform(&mut self, curve: &Pubkey, partner: &Pubkey) -> Pubkey {
        let platform = Pubkey::new_unique();
        let mut account = BondingCurve::from_bytes(&self.account(curve).data).unwrap();
        account.platform_config = solana_address::Address::new_from_array(platform.to_bytes());
        self.account_mut(curve).data = borsh::to_vec(&account).unwrap();
        let mut copy = self.account(&config_of(&PLATFORM, partner)).clone();
        let mut config = PartnerConfig::from_bytes(&copy.data).unwrap();
        config.platform_config = solana_address::Address::new_from_array(platform.to_bytes());
        copy.data = borsh::to_vec(&config).unwrap();
        self.0.insert(config_of(&platform, partner), copy);
        platform
    }

    /// Sets the creator fee rate that `market`, a curve or a pool, holds.
    fn set_creator_fee(&mut self, market: &Pubkey, creator_fee_bps: u16) {
        let account = self.account_mut(market);
        account.data = if account.owner == DEX_PROGRAM_ID {
            let mut pool = Pool::from_bytes(&account.data).unwrap();
            pool.creator_fee_bps = creator_fee_bps;
            borsh::to_vec(&pool).unwrap()
        } else {
            let mut curve = BondingCurve::from_bytes(&account.data).unwrap();
            curve.creator_fee_bps = creator_fee_bps;
            borsh::to_vec(&curve).unwrap()
        };
    }

    /// Sets `PARTNER`'s `max_creator_fee_bps` on both programs.
    fn set_max_creator_fee(&mut self, max_creator_fee_bps: u16) {
        let mut config =
            PartnerConfig::from_bytes(&self.account(&partner_config(&PLATFORM)).data).unwrap();
        config.launchpad.max_creator_fee_bps = max_creator_fee_bps;
        config.dex.max_creator_fee_bps = max_creator_fee_bps;
        self.account_mut(&partner_config(&PLATFORM)).data = borsh::to_vec(&config).unwrap();
    }

    fn set_epoch(&mut self, epoch: u64) {
        let clock = Clock {
            epoch,
            ..self.clock()
        };
        self.account_mut(&clock::ID).data = bincode::serialize(&clock).unwrap();
    }

    /// The payload of `mint`'s Token-2022 extension `wanted`.
    fn extension_mut(&mut self, mint: &Pubkey, wanted: u16) -> &mut [u8] {
        let data = &mut self.account_mut(mint).data;
        let mut offset = TLV_START;
        loop {
            let header: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
            let kind = u16::from_le_bytes([header[0], header[1]]);
            let len = usize::from(u16::from_le_bytes([header[2], header[3]]));
            assert_ne!(kind, 0, "{mint} has no extension {wanted}");
            if kind == wanted {
                return &mut data[offset + 4..offset + 4 + len];
            }
            offset += 4 + len;
        }
    }

    /// Caps `mint`'s current transfer fee at `maximum_fee`.
    fn set_maximum_fee(&mut self, mint: &Pubkey, maximum_fee: u64) {
        // `TransferFeeConfig`: two authorities, the withheld amount, the older
        // fee, then the newer `{epoch, maximum_fee, bps}`. The newer fee
        // applies at the snapshot's epoch.
        self.extension_mut(mint, TRANSFER_FEE_EXTENSION)[98..106]
            .copy_from_slice(&maximum_fee.to_le_bytes());
    }

    /// Points `mint`'s transfer hook at `program`, and writes its account list.
    fn arm_hook(&mut self, mint: &Pubkey, program: &Pubkey, list: Option<Vec<u8>>) {
        // `TransferHook`: the authority, then the program.
        self.extension_mut(mint, TRANSFER_HOOK_EXTENSION)[32..64].copy_from_slice(program.as_ref());
        if let Some(list) = list {
            let (validation, _) =
                Pubkey::find_program_address(&[b"extra-account-metas", mint.as_ref()], program);
            self.0.insert(
                validation,
                Account {
                    lamports: 10_000_000,
                    data: list,
                    owner: *program,
                    executable: false,
                    rent_epoch: 0,
                },
            );
        }
    }
}

/// A cache that holds only the keys a venue declared for its next update. It
/// fails a read of any other key, as Titan's cache may.
struct Declared<'a> {
    fixtures: &'a Fixtures,
    keys: Vec<Pubkey>,
}

#[async_trait]
impl AccountsCache for Declared<'_> {
    async fn get_account(&self, pubkey: &Pubkey) -> Result<Option<Account>, AccountCacheError> {
        let mut accounts = self.get_accounts(&[*pubkey]).await?;
        Ok(accounts.remove(0))
    }

    async fn get_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<Account>>, AccountCacheError> {
        if pubkeys.iter().any(|key| !self.keys.contains(key)) {
            return Err(AccountCacheError::FailedToAcquireReadLock);
        }
        self.fixtures.get_accounts(pubkeys).await
    }
}

/// `partner`'s config on `platform`.
fn config_of(platform: &Pubkey, partner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"partner", platform.as_ref(), partner.as_ref()],
        &NEXUS_PROGRAM,
    )
    .0
}

/// `PARTNER`'s config on `platform`.
fn partner_config(platform: &Pubkey) -> Pubkey {
    config_of(platform, &PARTNER)
}

/// Runs one update from a cache of the keys the venue declares.
async fn update_declared(venue: &mut SendfunVenue, fixtures: &Fixtures) {
    let keys = venue.get_required_pubkeys_for_update().unwrap();
    venue
        .update_state(&Declared { fixtures, keys })
        .await
        .unwrap();
}

/// Builds the venue from the market account, then updates it.
async fn venue(fixtures: &Fixtures, market: &Pubkey) -> SendfunVenue {
    let mut venue =
        SendfunVenue::from_account_with_partner(market, fixtures.account(market), &PARTNER)
            .unwrap();
    venue.update_state(fixtures).await.unwrap();
    venue
}

fn exact_in(input_mint: Pubkey, output_mint: Pubkey, amount: u64) -> QuoteRequest {
    QuoteRequest {
        input_mint,
        output_mint,
        amount,
        swap_type: SwapType::ExactIn,
    }
}

/// `(input, output)` mints of each direction the venue declares.
fn directions(venue: &SendfunVenue) -> Vec<(u8, u8, Pubkey, Pubkey)> {
    venue
        .directions_num()
        .into_iter()
        .map(|(input, output)| {
            (
                input,
                output,
                venue.get_token(input as usize).unwrap().pubkey,
                venue.get_token(output as usize).unwrap().pubkey,
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Titan's checks, as its shared suite runs them
// ---------------------------------------------------------------------------

/// Default seed for the sampling tests; override with `TEST_SEED=<u64>`.
const DEFAULT_TEST_SEED: u64 = 0x7174_616e_5345_4544; // "titanSED"

fn test_rng() -> rand::rngs::StdRng {
    use rand::SeedableRng;
    let seed = std::env::var("TEST_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TEST_SEED);
    rand::rngs::StdRng::seed_from_u64(seed)
}

/// Log-uniform sample in `[lo, hi]`, drawn from the supplied seeded RNG.
fn sample_log_uniform_u64(rng: &mut rand::rngs::StdRng, lo: u64, hi: u64) -> u64 {
    use rand::Rng;
    assert!(lo >= 1, "log-uniform sampling requires lo >= 1");
    assert!(lo <= hi);
    let log_lo = (lo as f64).ln();
    let log_hi = (hi as f64).ln();
    let r: f64 = rng.random();
    ((log_lo + r * (log_hi - log_lo)).exp() as u64).clamp(lo, hi)
}

/// Geometrically-spaced probe points across `[lb, ub]`, for the mean-value
/// theorem chord checks.
fn geometric_grid(lb: u64, ub: u64, n: usize) -> Vec<u64> {
    assert!(lb >= 1 && ub > lb && n >= 2);
    let ln_lo = (lb as f64).ln();
    let ln_hi = (ub as f64).ln();
    let mut points: Vec<u64> = (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            ((ln_lo + t * (ln_hi - ln_lo)).exp() as u64).clamp(lb, ub)
        })
        .collect();
    points.sort();
    points.dedup();
    points
}

/// Titan's `construction` check on one venue.
#[expect(
    clippy::result_large_err,
    reason = "the guarded closures return the template's `TradingVenueError`"
)]
fn construction(venue: &SendfunVenue) {
    let token_info = venue.get_token_info();
    assert!(
        token_info.len() >= 2,
        "venue must expose at least two tokens"
    );

    for (in_idx, out_idx) in venue.directions_num() {
        let (lower, upper) =
            assert_no_alloc(|| venue.bounds(in_idx, out_idx)).expect("boundary search failed");
        assert!(lower < upper, "lower bound must be < upper bound");

        let input_mint = venue.get_token(in_idx as usize).unwrap().pubkey;
        let output_mint = venue.get_token(out_idx as usize).unwrap().pubkey;

        for (edge, amount) in [("lower", lower), ("upper", upper)] {
            let q = assert_no_alloc(|| venue.quote(exact_in(input_mint, output_mint, amount)))
                .unwrap_or_else(|_| panic!("{edge}-bound quote failed"));
            assert!(
                !q.not_enough_liquidity,
                "{edge} bound: insufficient liquidity"
            );
            assert!(q.expected_output > 0, "{edge} bound: zero output");
            assert!(
                q.price > 0.0,
                "{edge} bound: non-positive price {}",
                q.price
            );
        }
    }
}

/// Titan's `monotone` check on one venue.
fn monotone(venue: &SendfunVenue) {
    let mut rng = test_rng();
    for (in_idx, out_idx) in venue.directions_num() {
        let (lb, ub) = venue.bounds(in_idx, out_idx).unwrap();
        let input_mint = venue.get_token(in_idx as usize).unwrap().pubkey;
        let output_mint = venue.get_token(out_idx as usize).unwrap().pubkey;

        let mut amounts: Vec<u64> = (0..50)
            .map(|_| sample_log_uniform_u64(&mut rng, lb, ub))
            .collect();
        amounts.sort();

        let mut prev = 0;
        for amount in amounts {
            let out = venue
                .quote(exact_in(input_mint, output_mint, amount))
                .expect("quote failed")
                .expected_output;
            assert!(
                prev <= out,
                "output not monotone: {prev} > {out} at {amount}"
            );
            prev = out;
        }
    }
}

/// Titan's `price_monotone` check on one venue.
fn price_monotone(venue: &SendfunVenue) {
    const REL_TOL: f64 = 1e-3; // slack so integer rounding can't look like a violation
    assert!(venue.get_token_info().len() >= 2);

    let mut rng = test_rng();
    for (in_idx, out_idx) in venue.directions_num() {
        let (lb, ub) = venue.bounds(in_idx, out_idx).unwrap();
        let input_mint = venue.get_token(in_idx as usize).unwrap().pubkey;
        let output_mint = venue.get_token(out_idx as usize).unwrap().pubkey;

        let mut amounts: Vec<u64> = (0..200)
            .map(|_| sample_log_uniform_u64(&mut rng, lb, ub))
            .collect();
        amounts.sort();

        let mut prev_price = f64::INFINITY;
        for amount in amounts {
            let price = venue
                .quote(exact_in(input_mint, output_mint, amount))
                .expect("quote failed")
                .price;
            assert!(
                price > 0.0,
                "price must be positive, got {price} at {amount}"
            );
            assert!(
                price <= prev_price * (1.0 + REL_TOL),
                "price not monotone non-increasing: {prev_price} -> {price} at {amount}"
            );
            prev_price = price;
        }
    }
}

/// Titan's `mean_value_theorem` check on one venue.
fn mean_value_theorem(venue: &SendfunVenue) {
    const REL_TOL: f64 = 1e-5; // 0.1 BPS
    const OUT_QUANTUM: f64 = 2.0; // two output atoms of floor-truncation slack
    assert!(venue.get_token_info().len() >= 2);

    for (in_idx, out_idx) in venue.directions_num() {
        let (lb, ub) = venue.bounds(in_idx, out_idx).unwrap();
        let input_mint = venue.get_token(in_idx as usize).unwrap().pubkey;
        let output_mint = venue.get_token(out_idx as usize).unwrap().pubkey;

        let grid = geometric_grid(lb, ub, 64);
        for pair in grid.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b <= a {
                continue;
            }
            let qa = venue
                .quote(exact_in(input_mint, output_mint, a))
                .expect("quote at a");
            let qb = venue
                .quote(exact_in(input_mint, output_mint, b))
                .expect("quote at b");
            if qb.expected_output <= qa.expected_output {
                continue; // flat step carries no rate information
            }

            let chord = (qb.expected_output - qa.expected_output) as f64 / (b - a) as f64;
            let (price_a, price_b) = (qa.price, qb.price); // f'(a) >= f'(b)
            let atol = OUT_QUANTUM / (b - a) as f64;

            assert!(
                price_b <= price_a * (1.0 + REL_TOL),
                "price increased with size: f'({a})={price_a} < f'({b})={price_b}"
            );
            assert!(
                chord <= price_a * (1.0 + REL_TOL) + atol,
                "chord {chord} exceeds left price {price_a} (atol {atol}) on [{a}, {b}]"
            );
            assert!(
                chord >= price_b * (1.0 - REL_TOL) - atol,
                "chord {chord} below right price {price_b} (atol {atol}) on [{a}, {b}]"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_snapshot_market_is_routable() {
    let expected = [
        (CURVE_WSOL, (6, true, None), (9, false, None)),
        (CURVE_FEE, (6, true, None), (9, true, Some((100, u64::MAX)))),
        (CURVE_SPL, (6, true, None), (6, false, None)),
        (POOL_WSOL, (6, true, None), (9, false, None)),
    ];
    for (market, base, quote) in expected {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        assert!(venue.initialized(), "{market}: {:?}", venue.halted());
        let tokens: Vec<_> = venue
            .get_token_info()
            .iter()
            .map(|token| {
                (
                    token.decimals,
                    token.is_token_2022,
                    token.transfer_fee.zip(token.maximum_fee),
                )
            })
            .collect();
        assert_eq!(tokens, vec![base, quote], "{market}");
    }
}

#[tokio::test]
async fn the_clock_is_read_only_while_it_moves_a_fee() {
    let fixtures = Fixtures::load(&CURVE_WSOL);
    let mut venue = SendfunVenue::from_account_with_partner(
        &CURVE_WSOL,
        fixtures.account(&CURVE_WSOL),
        &PARTNER,
    )
    .unwrap();
    let fixed = [
        CURVE_WSOL,
        CURVE_WSOL_BASE,
        WSOL,
        pubkey!("3jPJHu44ayVMWR36YjXum5NWMvLDiFmq8HAtxmXtennQ"),
        pubkey!("Fv5yAaFUmmVxo8LN3Q2T1jSwQ8gy7cdbJTYkMxaBDJzB"),
        partner_config(&PLATFORM),
    ];
    let first = venue.get_required_pubkeys_for_update().unwrap();
    assert_eq!(first, [fixed.as_slice(), &[clock::ID]].concat());
    venue.update_state(&fixtures).await.unwrap();
    // The decay window closed long ago, and no fee change is pending.
    assert_eq!(venue.get_required_pubkeys_for_update().unwrap(), fixed);

    // A later update without the clock keeps the last one.
    let mut without_clock = fixtures.clone();
    without_clock.0.remove(&clock::ID);
    venue.update_state(&without_clock).await.unwrap();
    assert!(venue.initialized());
}

/// An update that finds a curve created after the last clock reading reads
/// the clock, though the update did not declare it.
#[tokio::test]
async fn a_relaunched_curve_prices_at_the_current_clock() {
    let mut fixtures = Fixtures::load(&CURVE_WSOL);
    let mut venue = venue(&fixtures, &CURVE_WSOL).await;
    assert!(
        !venue
            .get_required_pubkeys_for_update()
            .unwrap()
            .contains(&clock::ID)
    );
    let now = fixtures.clock().unix_timestamp + 1_005;
    fixtures.relaunch(&CURVE_WSOL, now - 5);
    fixtures.set_unix_timestamp(now);
    venue.update_state(&fixtures).await.unwrap();
    // What the program pays for 1 SOL 5 seconds into the curve.
    let quote = venue
        .quote(exact_in(WSOL, CURVE_WSOL_BASE, 1_000_000_000))
        .unwrap();
    assert_eq!(quote.expected_output, 7_038_542_397_824);

    // The partner's premium decays over 20 seconds. Until then each
    // update reads the clock. After that no update reads it.
    let asks_for_clock = |venue: &SendfunVenue| {
        venue
            .get_required_pubkeys_for_update()
            .unwrap()
            .contains(&clock::ID)
    };
    assert!(asks_for_clock(&venue));
    fixtures.set_unix_timestamp(now + 14);
    venue.update_state(&fixtures).await.unwrap();
    assert!(asks_for_clock(&venue));
    fixtures.set_unix_timestamp(now + 15);
    venue.update_state(&fixtures).await.unwrap();
    assert!(!asks_for_clock(&venue));
}

/// A requested clock that is missing fails the update. The venue keeps its
/// last state and requests the clock again.
#[tokio::test]
async fn a_requested_clock_that_is_missing_fails_the_update() {
    // PRELWG's fee changes at epoch 1050, so at 1049 each update reads the
    // clock.
    let mut fixtures = Fixtures::load(&CURVE_FEE);
    fixtures.set_epoch(1_049);
    let mut venue = venue(&fixtures, &CURVE_FEE).await;
    fixtures.0.remove(&clock::ID);
    assert!(matches!(
        venue.update_state(&fixtures).await,
        Err(TradingVenueError::NoAccountFound(_))
    ));
    assert!(venue.initialized());
    assert!(
        venue
            .get_required_pubkeys_for_update()
            .unwrap()
            .contains(&clock::ID)
    );
}

#[tokio::test]
async fn zero_input_quotes_report_the_spot_price() {
    let mut spots = Vec::new();
    for market in MARKETS {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        for (_, _, input, output) in directions(&venue) {
            let quote = venue.quote(exact_in(input, output, 0)).unwrap();
            assert_eq!(
                (
                    quote.amount,
                    quote.expected_output,
                    quote.not_enough_liquidity
                ),
                (0, 0, false)
            );
            spots.push(quote.price);
        }
    }
    assert_eq!(
        spots,
        [
            0.000_097_631_625_200_478_66,
            9_836.976_471_792_785,
            0.000_004_057_911_946_276_031,
            231_963.643_485_124_25,
            0.000_003_166_989_120_310_713,
            303_253.331_007_899_1,
            0.000_406_299_810_899_015_03,
            2_453.858_513_480_339_3,
        ]
    );
}

#[tokio::test]
async fn bounds_and_edge_quotes_pass_titans_construction_check() {
    let mut bounds = Vec::new();
    for market in MARKETS {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        construction(&venue);
        for (input, output, _, _) in directions(&venue) {
            bounds.push(venue.bounds(input, output).unwrap());
        }
    }
    assert_eq!(
        bounds,
        [
            (1_000_000, 504_568_333_018_240),
            (1_000_000, 59_567_205_760),
            (1_000_000, 116_685_957_911_680),
            (1_000_000, 9_963_004_480),
            (1_000_000, 130_649_058_781_440),
            (1_000_000, 7_354_580_608),
            (1_000_000, 18_446_536_173_884_074_942),
            (1_000_000, 18_446_743_989_084_967_998),
        ]
    );
}

#[tokio::test]
async fn titans_pricing_checks_pass() {
    for market in MARKETS {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        monotone(&venue);
        price_monotone(&venue);
        mean_value_theorem(&venue);
    }
}

/// The price at `x` is within 0.1% of the central difference of the quotes
/// over `x ± x/1000`. The test checks each `x` where that window moves the
/// output by at least 10,000 atoms.
#[tokio::test]
async fn the_price_is_the_derivative_of_the_quote() {
    for market in MARKETS {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        for (input, output, input_mint, output_mint) in directions(&venue) {
            let (lower, upper) = venue.bounds(input, output).unwrap();
            let out = |amount| {
                venue
                    .quote(exact_in(input_mint, output_mint, amount))
                    .unwrap()
                    .expected_output
            };
            let mut checked = 0;
            for x in geometric_grid(lower, upper / 2, 64) {
                let h = x / 1_000;
                let rise = out(x + h) - out(x - h);
                if rise < 10_000 {
                    continue;
                }
                let difference = rise as f64 / (2 * h) as f64;
                let price = venue
                    .quote(exact_in(input_mint, output_mint, x))
                    .unwrap()
                    .price;
                assert!(
                    (price - difference).abs() / difference < 1e-3,
                    "{market} {input_mint} at {x}: price {price}, difference {difference}"
                );
                checked += 1;
            }
            assert!(checked >= 8, "{market} {input_mint}: {checked} points");
        }
    }
}

#[tokio::test]
async fn a_buy_past_the_supply_left_fills_the_rest() {
    let fixtures = Fixtures::load(&CURVE_WSOL);
    let venue = venue(&fixtures, &CURVE_WSOL).await;
    let request = exact_in(WSOL, CURVE_WSOL_BASE, 100_000_000_000);
    let quote = venue.quote(request.clone()).unwrap();
    assert!(quote.not_enough_liquidity);
    assert_eq!(
        (quote.amount, quote.expected_output),
        (59_567_205_793, 288_531_671_608_473)
    );
}

/// A trade pays the creator fee rate that its market holds. The
/// partner's `max_creator_fee_bps` limits that rate at the market's creation
/// only. No trade pays it, and a market's rate can be above it.
#[tokio::test]
async fn a_markets_creator_rate_prices_the_trade_not_the_partner_max() {
    // The partner's protocol rate here is 200 bps on the curve and 5 bps on
    // the pool, with no LP rate. Its max here is 50 bps on both. Each trade
    // pays the protocol rate plus the market's rate: 200, 230 and 280 bps on
    // the curve, 5, 15 and 85 bps on the pool.
    // Per rate: what 1 SOL buys, what 1,000,000 tokens sell for, and a buy's
    // spot price.
    type Expected = (u16, u64, u64, f64);
    let cases: [(Pubkey, Pubkey, [Expected; 3]); 2] = [
        (
            CURVE_WSOL,
            CURVE_WSOL_BASE,
            [
                (0, 9_669_638_840_005, 97_460_169, 9_836.976_471_792_785),
                (30, 9_640_539_933_454, 97_161_822, 9_806.863_278_511_786),
                (80, 9_592_035_020_186, 96_664_576, 9_756.674_623_043_455),
            ],
        ),
        (
            POOL_WSOL,
            POOL_WSOL_BASE,
            [
                (0, 2_427_633_802_902, 404_759_821, 2_456.316_058_311_066),
                (10, 2_425_233_288_145, 404_354_859, 2_453.858_513_480_339_3),
                (80, 2_408_428_114_200, 401_520_123, 2_436.655_699_665_254_4),
            ],
        ),
    ];
    for (market, base, expected) in cases {
        for (creator_fee_bps, bought, sold, spot) in expected {
            let mut fixtures = Fixtures::load(&market);
            fixtures.set_max_creator_fee(50);
            fixtures.set_creator_fee(&market, creator_fee_bps);
            let venue = venue(&fixtures, &market).await;
            let quote = |request| venue.quote(request).unwrap();
            assert_eq!(
                (
                    quote(exact_in(WSOL, base, 1_000_000_000)).expected_output,
                    quote(exact_in(base, WSOL, 1_000_000_000_000)).expected_output,
                    quote(exact_in(WSOL, base, 0)).price,
                ),
                (bought, sold, spot),
                "{market} at {creator_fee_bps} bps"
            );
        }
    }
}

#[tokio::test]
async fn the_transfer_fee_switches_at_its_epoch() {
    // PRELWG charges 300 bps before epoch 1050 and 100 bps from it.
    let mut fixtures = Fixtures::load(&CURVE_FEE);
    fixtures.set_epoch(1_049);
    let mut venue = venue(&fixtures, &CURVE_FEE).await;
    let quote_info = |venue: &SendfunVenue| {
        let token = venue.get_token(1).unwrap();
        (token.pubkey, token.transfer_fee, token.maximum_fee)
    };
    assert_eq!(quote_info(&venue), (PRELWG, Some(300), Some(u64::MAX)));
    // The fee changes at a later epoch, so the venue requests the clock.
    assert!(
        venue
            .get_required_pubkeys_for_update()
            .unwrap()
            .contains(&clock::ID)
    );
    let buy = exact_in(PRELWG, CURVE_FEE_BASE, 1_000_000_000);
    let before = venue.quote(buy.clone()).unwrap().expected_output;

    fixtures.set_epoch(1_050);
    venue.update_state(&fixtures).await.unwrap();
    assert_eq!(quote_info(&venue), (PRELWG, Some(100), Some(u64::MAX)));
    assert!(
        !venue
            .get_required_pubkeys_for_update()
            .unwrap()
            .contains(&clock::ID)
    );
    let after = venue.quote(buy).unwrap().expected_output;
    assert_eq!((before, after), (183_634_864_017_590, 186_682_029_150_576));
}

/// A hook's account list under its `Execute` entry.
fn account_list(metas: &[ExtraAccountMeta]) -> Vec<u8> {
    struct Execute;
    impl SplDiscriminate for Execute {
        const SPL_DISCRIMINATOR: ArrayDiscriminator =
            ArrayDiscriminator::new([105, 37, 101, 197, 75, 251, 102, 26]);
    }
    let mut data = vec![0; ExtraAccountMetaList::size_of(metas.len()).unwrap()];
    ExtraAccountMetaList::init::<Execute>(&mut data, metas).unwrap();
    data
}

/// The account list of `spl-transfer-hook-example`: a fixed account, a
/// writable PDA keyed on the source, and a PDA keyed on the source's owner,
/// read from the source's data.
fn hook_list() -> Vec<u8> {
    account_list(&[
        ExtraAccountMeta::new_with_pubkey(
            &solana_address::Address::new_from_array(RENT_SYSVAR.to_bytes()),
            false,
            false,
        )
        .unwrap(),
        ExtraAccountMeta::new_with_seeds(
            &[
                Seed::Literal {
                    bytes: b"ledger".to_vec(),
                },
                Seed::AccountKey { index: 0 },
            ],
            false,
            true,
        )
        .unwrap(),
        ExtraAccountMeta::new_with_seeds(
            &[
                Seed::Literal {
                    bytes: b"owner".to_vec(),
                },
                Seed::AccountData {
                    account_index: 0,
                    data_index: 32,
                    length: 32,
                },
            ],
            false,
            false,
        )
        .unwrap(),
    ])
}

/// A list of one read-only extra: the owner of `Execute` account
/// `account_index`, a token account, read from its data.
fn owner_hook_list(account_index: u8) -> Vec<u8> {
    account_list(&[ExtraAccountMeta::new_with_pubkey_data(
        &PubkeyData::AccountData {
            account_index,
            data_index: 32,
        },
        false,
        false,
    )
    .unwrap()])
}

/// The quote mint's hook runs on the quote transfer: into the vault on a buy,
/// out of it on a sell. Its extras resolve from the snapshot. They follow the
/// 17 trade accounts, then the hook program and its list.
#[tokio::test]
async fn a_hooked_quote_mint_routes_with_its_extra_accounts() {
    let mut fixtures = Fixtures::load(&CURVE_FEE);
    fixtures.arm_hook(&PRELWG, &HOOK_PROGRAM, Some(hook_list()));
    let venue = venue(&fixtures, &CURVE_FEE).await;
    assert!(venue.initialized(), "{:?}", venue.halted());
    assert_eq!(
        venue.program_dependencies(),
        [
            LAUNCHPAD_PROGRAM_ID,
            TOKEN_PROGRAM,
            TOKEN_2022_PROGRAM,
            ATA_PROGRAM,
            SYSTEM_PROGRAM,
            HOOK_PROGRAM
        ]
    );
    let user = Pubkey::new_unique();
    let curve = BondingCurve::from_bytes(&fixtures.account(&CURVE_FEE).data).unwrap();
    let quote_vault = Pubkey::new_from_array(curve.quote_vault.to_bytes());
    let user_quote =
        get_associated_token_address_with_program_id(&user, &PRELWG, &TOKEN_2022_PROGRAM);
    let pda = |seeds: &[&[u8]]| Pubkey::find_program_address(seeds, &HOOK_PROGRAM).0;
    let validation = pda(&[b"extra-account-metas", PRELWG.as_ref()]);
    // The quote transfer's source and its owner.
    for (request, source, owner) in [
        (
            exact_in(PRELWG, CURVE_FEE_BASE, 1_000_000_000),
            user_quote,
            user,
        ),
        (
            exact_in(CURVE_FEE_BASE, PRELWG, 1_000_000_000_000),
            quote_vault,
            CURVE_FEE,
        ),
    ] {
        let accounts = venue
            .generate_swap_instruction(request, user)
            .unwrap()
            .accounts;
        assert_eq!(
            accounts[17..],
            [
                AccountMeta::new_readonly(RENT_SYSVAR, false),
                AccountMeta::new(pda(&[b"ledger", source.as_ref()]), false),
                AccountMeta::new_readonly(pda(&[b"owner", owner.as_ref()]), false),
                AccountMeta::new_readonly(HOOK_PROGRAM, false),
                AccountMeta::new_readonly(validation, false),
            ]
        );
    }
}

/// A hook can name the owner of either token account of its transfer: the
/// trader or the market. Both forward read-only, after the 17 trade accounts.
#[tokio::test]
async fn a_hook_naming_a_transfer_owner_routes_both_ways() {
    let user = Pubkey::new_unique();
    let (validation, _) =
        Pubkey::find_program_address(&[b"extra-account-metas", PRELWG.as_ref()], &HOOK_PROGRAM);
    // `Execute` account 0 is the source, 2 the destination. A buy moves the
    // quote mint from the user to the market, a sell from the market to the
    // user.
    for (account_index, buy_owner, sell_owner) in [(0, user, CURVE_FEE), (2, CURVE_FEE, user)] {
        let mut fixtures = Fixtures::load(&CURVE_FEE);
        fixtures.arm_hook(&PRELWG, &HOOK_PROGRAM, Some(owner_hook_list(account_index)));
        let venue = venue(&fixtures, &CURVE_FEE).await;
        assert!(venue.initialized(), "{:?}", venue.halted());
        for (request, owner) in [
            (exact_in(PRELWG, CURVE_FEE_BASE, 1_000_000_000), buy_owner),
            (
                exact_in(CURVE_FEE_BASE, PRELWG, 1_000_000_000_000),
                sell_owner,
            ),
        ] {
            let accounts = venue
                .generate_swap_instruction(request, user)
                .unwrap()
                .accounts;
            assert_eq!(
                accounts[..2],
                [
                    AccountMeta::new_readonly(user, true),
                    AccountMeta::new_readonly(user, true)
                ]
            );
            assert_eq!(
                accounts[17..],
                [
                    AccountMeta::new_readonly(owner, false),
                    AccountMeta::new_readonly(HOOK_PROGRAM, false),
                    AccountMeta::new_readonly(validation, false),
                ]
            );
        }
    }
}

/// Below the lower bound, a sell can round to no output. The quote refuses
/// it. The programs accept the input and pay nothing. Each amount is the
/// smallest sell that prices 1 atom of quote, which the platform fee takes.
#[tokio::test]
async fn a_trade_that_delivers_nothing_is_refused() {
    for (market, base, quote_mint, amount) in [
        (CURVE_WSOL, CURVE_WSOL_BASE, WSOL, 10_038),
        (CURVE_FEE, CURVE_FEE_BASE, PRELWG, 239_089),
        (POOL_WSOL, POOL_WSOL_BASE, WSOL, 2_458),
    ] {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        let quote = venue.quote(exact_in(base, quote_mint, amount));
        assert!(
            matches!(
                quote,
                Err(TradingVenueError::AmmMethodError(ErrorInfo::StaticStr(
                    "swap delivers nothing"
                )))
            ),
            "{market} sell of {amount}: {quote:?}"
        );
    }
}

/// Asserts that `venue` halts for `reason`, and that `bounds`, `quote` and
/// `generate_swap_instruction` fail.
fn assert_unroutable(venue: &SendfunVenue, reason: &str) {
    assert!(!venue.initialized());
    assert_eq!(venue.halted(), Some(reason));
    for (input, output, input_mint, output_mint) in directions(venue) {
        assert!(matches!(
            venue.bounds(input, output),
            Err(TradingVenueError::InactivePoolError(..))
        ));
        let request = exact_in(input_mint, output_mint, 1_000_000_000);
        assert!(matches!(
            venue.quote(request.clone()),
            Err(TradingVenueError::InactivePoolError(..))
        ));
        assert!(
            venue
                .generate_swap_instruction(request, Pubkey::new_unique())
                .is_err()
        );
    }
}

/// A curve with no buys holds no quote, so no sell quotes. The venue declares
/// only the buy, and passes Titan's construction check.
#[tokio::test]
async fn a_curve_with_no_buys_declares_only_the_buy() {
    let mut fixtures = Fixtures::load(&CURVE_WSOL);
    fixtures.undo_buys(&CURVE_WSOL);
    let venue = venue(&fixtures, &CURVE_WSOL).await;
    assert!(venue.initialized(), "{:?}", venue.halted());
    // Token 0 is the base mint, token 1 the quote mint.
    assert!(matches!(
        venue.bounds(0, 1),
        Err(TradingVenueError::NoQuotableValue(_))
    ));
    assert_eq!(venue.directions_num(), vec![(1, 0)]);
    construction(&venue);
}

/// The venue names its mints before its first update: base, then quote.
#[test]
fn the_mints_are_known_before_the_first_update() {
    let fixtures = Fixtures::load(&CURVE_WSOL);
    let venue = SendfunVenue::from_account_with_partner(
        &CURVE_WSOL,
        fixtures.account(&CURVE_WSOL),
        &PARTNER,
    )
    .unwrap();
    assert!(!venue.initialized());
    assert_eq!(venue.tradable_mints().unwrap(), vec![CURVE_WSOL_BASE, WSOL]);
}

/// A reason the venue gives, the market, and the change to its snapshot.
type Derived = (&'static str, Pubkey, fn(&mut Fixtures));

#[tokio::test]
async fn untradable_markets_are_unroutable() {
    let cases: [Derived; 7] = [
        (
            "the partner has no config on this market's platform",
            CURVE_WSOL,
            |fixtures| {
                // Anyone can fund the address. Only nexus writes a config.
                fixtures.account_mut(&partner_config(&PLATFORM)).owner = SYSTEM_PROGRAM;
            },
        ),
        ("bonding curve is paused", CURVE_WSOL, |fixtures| {
            fixtures.account_mut(&CURVE_WSOL).data[STATUS_OFFSET] = 3;
        }),
        ("bonding curve is complete", CURVE_WSOL, |fixtures| {
            fixtures.account_mut(&CURVE_WSOL).data[STATUS_OFFSET] = 1;
        }),
        ("pool is paused", POOL_WSOL, |fixtures| {
            fixtures.account_mut(&POOL_WSOL).data[STATUS_OFFSET] = 1;
        }),
        (
            "the partner has no config on this market's platform",
            POOL_WSOL,
            |fixtures| {
                // The System Program key's config is no fallback.
                let config = fixtures.0.remove(&partner_config(&PLATFORM)).unwrap();
                fixtures.0.insert(SNAPSHOT_PARTNER_CONFIG, config);
            },
        ),
        ("a mint is paused", CURVE_FEE, |fixtures| {
            // `PausableConfig`: the authority, then the `paused` flag.
            fixtures.extension_mut(&PRELWG, PAUSABLE_EXTENSION)[32] = 1;
        }),
        ("a mint freezes new token accounts", CURVE_FEE, |fixtures| {
            // `AccountState::Frozen`.
            fixtures.extension_mut(&PRELWG, DEFAULT_ACCOUNT_STATE_EXTENSION)[0] = 2;
        }),
    ];
    for (reason, market, change) in cases {
        let mut fixtures = Fixtures::load(&market);
        change(&mut fixtures);
        let venue = venue(&fixtures, &market).await;
        assert_unroutable(&venue, reason);
    }
}

#[tokio::test]
async fn a_closed_curve_is_unroutable_until_it_returns() {
    let fixtures = Fixtures::load(&CURVE_WSOL);
    let mut venue = venue(&fixtures, &CURVE_WSOL).await;
    let mut closed = fixtures.clone();
    closed.0.remove(&CURVE_WSOL);
    venue.update_state(&closed).await.unwrap();
    assert!(!venue.initialized());
    assert_eq!(venue.halted(), Some("market account is closed"));
    venue.update_state(&fixtures).await.unwrap();
    assert!(venue.initialized());
}

#[tokio::test]
async fn a_hook_without_a_list_is_unroutable() {
    let mut fixtures = Fixtures::load(&CURVE_FEE);
    fixtures.arm_hook(&PRELWG, &HOOK_PROGRAM, None);
    let mut venue = venue(&fixtures, &CURVE_FEE).await;
    assert_unroutable(&venue, "transfer hook accounts not loaded");
    // The list's address is part of the next update.
    let (validation, _) =
        Pubkey::find_program_address(&[b"extra-account-metas", PRELWG.as_ref()], &HOOK_PROGRAM);
    assert!(
        venue
            .get_required_pubkeys_for_update()
            .unwrap()
            .contains(&validation)
    );
    venue.update_state(&fixtures).await.unwrap();
    assert_unroutable(&venue, "a transfer hook cannot be resolved before the swap");
}

/// An update that needs an undeclared account, from a cache that refuses it,
/// leaves the market unroutable and declares the account. The next update
/// routes the market.
#[tokio::test]
async fn an_undeclared_account_delays_routing_by_one_update() {
    let cases: [Derived; 3] = [
        ("transfer hook accounts not loaded", CURVE_FEE, |fixtures| {
            fixtures.arm_hook(&PRELWG, &HOOK_PROGRAM, Some(hook_list()));
        }),
        ("partner config not loaded", CURVE_WSOL, |fixtures| {
            fixtures.move_platform(&CURVE_WSOL, &PARTNER);
        }),
        ("clock not loaded", CURVE_WSOL, |fixtures| {
            let now = fixtures.clock().unix_timestamp + 1_005;
            fixtures.relaunch(&CURVE_WSOL, now - 5);
            fixtures.set_unix_timestamp(now);
        }),
    ];
    for (reason, market, change) in cases {
        let mut fixtures = Fixtures::load(&market);
        let mut venue = venue(&fixtures, &market).await;
        change(&mut fixtures);
        update_declared(&mut venue, &fixtures).await;
        assert_eq!(venue.halted(), Some(reason));
        update_declared(&mut venue, &fixtures).await;
        assert!(venue.initialized(), "{reason}: {:?}", venue.halted());
    }
}

/// A cache that fails an account the update declared, here a hook's account
/// list, fails the update. The venue keeps its last state.
#[tokio::test]
async fn a_cache_error_on_a_declared_account_keeps_the_last_state() {
    let mut fixtures = Fixtures::load(&CURVE_FEE);
    fixtures.arm_hook(&PRELWG, &HOOK_PROGRAM, Some(hook_list()));
    let mut venue = venue(&fixtures, &CURVE_FEE).await;
    let (validation, _) =
        Pubkey::find_program_address(&[b"extra-account-metas", PRELWG.as_ref()], &HOOK_PROGRAM);
    let declared = venue.get_required_pubkeys_for_update().unwrap();
    assert!(declared.contains(&validation));
    let keys = declared
        .into_iter()
        .filter(|key| *key != validation)
        .collect();
    assert!(matches!(
        venue
            .update_state(&Declared {
                fixtures: &fixtures,
                keys
            })
            .await,
        Err(TradingVenueError::AccountCacheError(_))
    ));
    assert!(venue.initialized(), "{:?}", venue.halted());
}

/// An account that does not decode fails the update and halts the venue. An
/// update that decodes it routes the market again.
#[tokio::test]
async fn an_account_that_does_not_decode_halts_the_venue() {
    let mut fixtures = Fixtures::load(&POOL_WSOL);
    let mut venue = venue(&fixtures, &POOL_WSOL).await;
    let data = fixtures.account(&POOL_WSOL).data.clone();
    fixtures.account_mut(&POOL_WSOL).data.truncate(8);
    assert!(matches!(
        venue.update_state(&fixtures).await,
        Err(TradingVenueError::DeserializationFailed(_))
    ));
    assert_unroutable(&venue, "an account does not decode");
    fixtures.account_mut(&POOL_WSOL).data = data;
    venue.update_state(&fixtures).await.unwrap();
    assert!(venue.initialized(), "{:?}", venue.halted());
}

#[tokio::test]
async fn titan_routes_exact_in_only() {
    let fixtures = Fixtures::load(&POOL_WSOL);
    let venue = venue(&fixtures, &POOL_WSOL).await;
    let exact_out = QuoteRequest {
        swap_type: SwapType::ExactOut,
        ..exact_in(WSOL, POOL_WSOL_BASE, 1_000_000)
    };
    assert!(matches!(
        venue.quote(exact_out.clone()),
        Err(TradingVenueError::ExactOutNotSupported)
    ));
    assert!(matches!(
        venue.generate_swap_instruction(exact_out, Pubkey::new_unique()),
        Err(TradingVenueError::ExactOutNotSupported)
    ));
    let stranger = Pubkey::new_unique();
    assert!(matches!(
        venue.quote(exact_in(stranger, WSOL, 1_000_000)),
        Err(TradingVenueError::InvalidMint(_))
    ));
    assert!(matches!(
        venue.quote(exact_in(WSOL, WSOL, 1_000_000)),
        Err(TradingVenueError::InvalidMint(_))
    ));
}

/// The default partner is the System Program key. A venue routes as it only
/// when it is named. It reads the default partner's config, pays its
/// schedule and does not sign, in the trade and in the route leg.
#[tokio::test]
async fn a_venue_named_the_default_partner_routes_without_a_signature() {
    // What 1 SOL buys, at the default partner's 200 bps on the curve and
    // 5 bps on the pool plus the pool's 10 bps creator fee.
    for (market, base, bought) in [
        (CURVE_WSOL, CURVE_WSOL_BASE, 9_669_638_840_005),
        (POOL_WSOL, POOL_WSOL_BASE, 2_425_233_288_145),
    ] {
        let fixtures = Fixtures::snapshot(&market);
        let mut venue = SendfunVenue::from_account_with_partner(
            &market,
            fixtures.account(&market),
            &SYSTEM_PROGRAM,
        )
        .unwrap();
        assert_eq!(venue.partner(), SYSTEM_PROGRAM);
        assert_eq!(
            venue.get_required_pubkeys_for_update().unwrap()[5],
            SNAPSHOT_PARTNER_CONFIG
        );
        update_declared(&mut venue, &fixtures).await;
        assert!(venue.initialized(), "{market}: {:?}", venue.halted());
        construction(&venue);

        let buy = exact_in(WSOL, base, 1_000_000_000);
        assert_eq!(venue.quote(buy.clone()).unwrap().expected_output, bought);
        let user = Pubkey::new_unique();
        let trade = venue.generate_swap_instruction(buy.clone(), user).unwrap();
        assert_eq!(
            trade.accounts[9..11],
            [
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
                AccountMeta::new_readonly(SNAPSHOT_PARTNER_CONFIG, false)
            ]
        );
        let (_, leg) = build_swap_leg(&venue, &buy, user, 0, 1, ROUTE_WEIGHT_ALL).unwrap();
        assert_eq!(leg[9], AccountMeta::new_readonly(SYSTEM_PROGRAM, false));

        // A registered partner with no config here is unroutable. The default
        // partner's config is no fallback.
        let mut registered =
            SendfunVenue::from_account_with_partner(&market, fixtures.account(&market), &PARTNER)
                .unwrap();
        registered.update_state(&fixtures).await.unwrap();
        assert_eq!(
            registered.halted(),
            Some("the partner has no config on this market's platform")
        );
    }
}

/// The programs refuse a partner config that names another partner or
/// another platform. The venue does not route on one.
#[tokio::test]
async fn a_config_of_another_partner_or_platform_is_no_config() {
    let other = solana_address::Address::new_from_array(HOOK_PROGRAM.to_bytes());
    let changes: [fn(&mut PartnerConfig, solana_address::Address); 2] = [
        |config, other| config.partner = other,
        |config, other| config.platform_config = other,
    ];
    for change in changes {
        let mut fixtures = Fixtures::load(&CURVE_WSOL);
        let key = partner_config(&PLATFORM);
        let mut config = PartnerConfig::from_bytes(&fixtures.account(&key).data).unwrap();
        change(&mut config, other);
        fixtures.account_mut(&key).data = borsh::to_vec(&config).unwrap();
        let venue = venue(&fixtures, &CURVE_WSOL).await;
        assert_eq!(
            venue.halted(),
            Some("the partner has no config on this market's platform")
        );
    }
}

/// `from_account` routes as the partner that `set_partner` names. A second
/// key is refused.
#[test]
fn from_account_routes_as_the_partner_that_is_set() {
    assert_eq!(set_partner(PARTNER), Ok(()));
    assert_eq!(set_partner(PARTNER), Ok(()));
    for other in [SYSTEM_PROGRAM, HOOK_PROGRAM] {
        assert_eq!(set_partner(other), Err(PARTNER));
    }
    let fixtures = Fixtures::load(&CURVE_WSOL);
    let venue = SendfunVenue::from_account(&CURVE_WSOL, fixtures.account(&CURVE_WSOL)).unwrap();
    assert_eq!(venue.partner(), PARTNER);
    assert_eq!(
        venue.get_required_pubkeys_for_update().unwrap()[5],
        partner_config(&PLATFORM)
    );
}

/// A trade pays the protocol rate of the partner's own config: its launchpad
/// rate on a curve and its DEX rate on a pool.
#[tokio::test]
async fn the_partners_schedule_prices_the_trade() {
    // What 1 SOL buys at each protocol rate, with the decay window closed. The
    // curve has no creator fee. The pool adds its 10 bps creator fee.
    type Rates = [(u16, u64); 2];
    let cases: [(Pubkey, Pubkey, Rates); 2] = [
        (
            CURVE_WSOL,
            CURVE_WSOL_BASE,
            [(200, 9_669_638_840_005), (0, 9_863_554_127_627)],
        ),
        (
            POOL_WSOL,
            POOL_WSOL_BASE,
            [(5, 2_425_233_288_145), (0, 2_426_433_552_535)],
        ),
    ];
    for (market, base, rates) in cases {
        for (protocol_fee_bps, bought) in rates {
            let mut fixtures = Fixtures::load(&market);
            let key = partner_config(&PLATFORM);
            let mut config = PartnerConfig::from_bytes(&fixtures.account(&key).data).unwrap();
            // The other program's rate is a decoy.
            let (own, other) = if market == POOL_WSOL {
                (
                    &mut config.dex.protocol_fee_bps,
                    &mut config.launchpad.protocol_fee_bps,
                )
            } else {
                (
                    &mut config.launchpad.protocol_fee_bps,
                    &mut config.dex.protocol_fee_bps,
                )
            };
            *own = protocol_fee_bps;
            *other = 777;
            fixtures.account_mut(&key).data = borsh::to_vec(&config).unwrap();
            let venue = venue(&fixtures, &market).await;
            assert_eq!(
                venue
                    .quote(exact_in(WSOL, base, 1_000_000_000))
                    .unwrap()
                    .expected_output,
                bought,
                "{market} at {protocol_fee_bps} bps"
            );
        }
    }
}

/// A relaunch can move a curve to another platform. The venue then reads its
/// partner's config on the new platform, as a registered partner and as the
/// default partner. The config on the old platform stays, and is not used.
#[tokio::test]
async fn a_moved_curve_reads_its_partners_config_on_the_new_platform() {
    for (mut fixtures, key) in [
        (Fixtures::load(&CURVE_WSOL), PARTNER),
        (Fixtures::snapshot(&CURVE_WSOL), SYSTEM_PROGRAM),
    ] {
        let mut venue = SendfunVenue::from_account_with_partner(
            &CURVE_WSOL,
            fixtures.account(&CURVE_WSOL),
            &key,
        )
        .unwrap();
        venue.update_state(&fixtures).await.unwrap();
        assert_eq!(
            venue.get_required_pubkeys_for_update().unwrap()[5],
            config_of(&PLATFORM, &key)
        );

        let platform = fixtures.move_platform(&CURVE_WSOL, &key);
        venue.update_state(&fixtures).await.unwrap();
        assert!(venue.initialized(), "{key}: {:?}", venue.halted());
        let moved = config_of(&platform, &key);
        assert_eq!(venue.get_required_pubkeys_for_update().unwrap()[5], moved);
        let buy = exact_in(WSOL, CURVE_WSOL_BASE, 1_000_000_000);
        assert_eq!(
            venue.quote(buy.clone()).unwrap().expected_output,
            9_669_638_840_005
        );
        let trade = venue
            .generate_swap_instruction(buy, Pubkey::new_unique())
            .unwrap();
        assert_eq!(
            trade.accounts[9..11],
            [
                AccountMeta::new_readonly(key, key == PARTNER),
                AccountMeta::new_readonly(moved, false)
            ]
        );
    }
}

#[tokio::test]
async fn the_swap_instruction_is_the_programs_exact_in_trade() {
    let fixtures = Fixtures::load(&POOL_WSOL);
    let venue = venue(&fixtures, &POOL_WSOL).await;
    let user = Pubkey::new_unique();
    let sell = venue
        .generate_swap_instruction(exact_in(POOL_WSOL_BASE, WSOL, 1_234_567), user)
        .unwrap();
    let mut data = vec![149, 39, 222, 155, 211, 124, 152, 26];
    data.extend_from_slice(&1_234_567_u64.to_le_bytes());
    data.extend_from_slice(&0_u64.to_le_bytes());
    let user_ata =
        |mint, program| get_associated_token_address_with_program_id(&user, &mint, &program);
    let (event_authority, _) =
        Pubkey::find_program_address(&[b"__event_authority"], &DEX_PROGRAM_ID);
    assert_eq!(
        sell,
        Instruction {
            program_id: DEX_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new_readonly(user, true),
                AccountMeta::new_readonly(user, true),
                AccountMeta::new(POOL_WSOL, false),
                AccountMeta::new_readonly(POOL_WSOL_BASE, false),
                AccountMeta::new_readonly(WSOL, false),
                AccountMeta::new(
                    pubkey!("GnuVezHmHa6ygKarBRHA9v8JAJmouAo1qSLHojPRC8Qu"),
                    false
                ),
                AccountMeta::new(
                    pubkey!("8iGyLuixYBXrgNtnf4LLcSTcYPr3RjadQYAfGedToiLM"),
                    false
                ),
                AccountMeta::new(user_ata(POOL_WSOL_BASE, TOKEN_2022_PROGRAM), false),
                AccountMeta::new(user_ata(WSOL, TOKEN_PROGRAM), false),
                // The partner signs.
                AccountMeta::new_readonly(PARTNER, true),
                AccountMeta::new_readonly(partner_config(&PLATFORM), false),
                AccountMeta::new_readonly(TOKEN_2022_PROGRAM, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM, false),
                AccountMeta::new_readonly(ATA_PROGRAM, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
                AccountMeta::new_readonly(event_authority, false),
                AccountMeta::new_readonly(DEX_PROGRAM_ID, false),
            ],
            data,
        }
    );
}

/// The route leg that Titan's router CPIs: TitanPDA is the user and the payer.
/// It is not a signer and is read-only, so the leg takes no write lock on it.
/// The program id is last.
#[tokio::test]
async fn a_route_leg_names_the_program_and_direction() {
    let titan_pda = Pubkey::new_unique();
    for (market, base, dex) in [
        (CURVE_WSOL, CURVE_WSOL_BASE, false),
        (POOL_WSOL, POOL_WSOL_BASE, true),
    ] {
        let fixtures = Fixtures::load(&market);
        let venue = venue(&fixtures, &market).await;
        for (request, buy) in [
            (exact_in(WSOL, base, 1_000_000_000), true),
            (exact_in(base, WSOL, 1_000_000_000), false),
        ] {
            let (spec, accounts) =
                build_swap_leg(&venue, &request, titan_pda, 0, 1, ROUTE_WEIGHT_ALL).unwrap();
            assert_eq!(spec.venue, Venue::Sendfun { dex, buy });
            assert_eq!(spec.n_accounts, 18);
            assert_eq!(
                accounts.last().unwrap(),
                &AccountMeta::new_readonly(venue.program_id(), false)
            );
            assert_eq!(
                accounts[..2],
                [
                    AccountMeta::new_readonly(titan_pda, false),
                    AccountMeta::new_readonly(titan_pda, false)
                ]
            );
            // The leg keeps the partner a signer.
            assert_eq!(accounts[9], AccountMeta::new_readonly(PARTNER, true));
        }
    }
}

/// Each direction routes the wider side of a capped transfer fee's cap point
/// that passes Titan's checks. The spot price uses the fee rate at the start
/// of the range. It is at or above the price at the lower bound.
#[tokio::test]
async fn a_capped_transfer_fee_routes_its_wider_side() {
    // PRELWG's fee is 100 bps, `ceil(x / 100)`. Capped at 100,000 atoms, it is
    // flat from a transfer of 9,999,901, which lands 9,899,901: a buy's range
    // starts at that transfer, a sell's where its payout first passes it, at
    // 9,899,902. Capped at 1 atom, it is flat from a transfer of 1. Each
    // range runs to the supply left, or the quote the curve holds.
    // Per direction, sell then buy: (bounds, payout at the lower bound, spot
    // price, price at the lower bound).
    type Expected = ((u64, u64), u64, f64, f64);
    let cases: [(u64, [Expected; 2]); 2] = [
        (
            100_000,
            [
                (
                    (2_445_894_273_788, 116_685_957_911_680),
                    9_899_902,
                    0.000_004_098_900_955_834_375,
                    0.000_004_078_014_204_136_977,
                ),
                (
                    (9_999_901, 9_863_474_432),
                    2_314_000_219_297,
                    234_306.710_591_034_6,
                    233_174.174_949_723_7,
                ),
            ],
        ),
        (
            1,
            [
                (
                    (1_000_000, 116_685_957_911_680),
                    2,
                    0.000_004_098_900_955_834_375,
                    0.000_004_098_900_947_262_084,
                ),
                (
                    (1_000_000, 9_863_374_464),
                    234_249_078_143,
                    234_306.710_591_034_6,
                    234_191.937_812_507_39,
                ),
            ],
        ),
    ];
    for (maximum_fee, expected) in cases {
        let mut fixtures = Fixtures::load(&CURVE_FEE);
        fixtures.set_maximum_fee(&PRELWG, maximum_fee);
        let venue = venue(&fixtures, &CURVE_FEE).await;
        construction(&venue);
        monotone(&venue);
        price_monotone(&venue);
        mean_value_theorem(&venue);
        let actual: Vec<Expected> = directions(&venue)
            .into_iter()
            .map(|(input, output, input_mint, output_mint)| {
                let (lower, upper) = venue.bounds(input, output).unwrap();
                let spot = venue.quote(exact_in(input_mint, output_mint, 0)).unwrap();
                let at_lower = venue
                    .quote(exact_in(input_mint, output_mint, lower))
                    .unwrap();
                (
                    (lower, upper),
                    at_lower.expected_output,
                    spot.price,
                    at_lower.price,
                )
            })
            .collect();
        assert_eq!(actual, expected, "maximum fee {maximum_fee}");

        // Each upper bound sits at an edge: the supply left, or the quote the
        // curve holds.
        let (_, buy_upper) = expected[1].0;
        let past_buy = venue
            .quote(exact_in(PRELWG, CURVE_FEE_BASE, buy_upper + 100))
            .unwrap();
        assert!(past_buy.not_enough_liquidity);
        let (_, sell_upper) = expected[0].0;
        assert!(
            venue
                .quote(exact_in(CURVE_FEE_BASE, PRELWG, sell_upper + 100))
                .is_err()
        );
    }
}

/// Each upper bound is within 100 atoms of an edge. A buy past it is
/// supply-capped. A sell past it fails.
#[tokio::test]
async fn each_upper_bound_sits_at_an_edge() {
    let fixtures = Fixtures::load(&CURVE_WSOL);
    let venue = venue(&fixtures, &CURVE_WSOL).await;
    let (_, buy_upper) = venue.bounds(1, 0).unwrap();
    let past_buy = venue
        .quote(exact_in(WSOL, CURVE_WSOL_BASE, buy_upper + 100))
        .unwrap();
    assert!(past_buy.not_enough_liquidity);
    let (_, sell_upper) = venue.bounds(0, 1).unwrap();
    assert!(matches!(
        venue.quote(exact_in(CURVE_WSOL_BASE, WSOL, sell_upper + 100)),
        Err(TradingVenueError::AmmMethodError(_))
    ));
}

/// The creation parser finds each snapshot market in the instruction that
/// created it: a curve's `create_token`, as the SDK builds it, and the
/// `create_pool` CPI that migrated a curve into the pool.
#[test]
fn creations_parse_to_the_snapshot_markets() {
    let address = |key: &Pubkey| solana_address::Address::new_from_array(key.to_bytes());
    let pubkey = |key: &solana_address::Address| Pubkey::new_from_array(key.to_bytes());
    for market in [CURVE_WSOL, CURVE_FEE, CURVE_SPL] {
        let fixtures = Fixtures::load(&market);
        let curve = BondingCurve::from_bytes(&fixtures.account(&market).data).unwrap();
        let user = address(&Pubkey::new_unique());
        let create = build_create_token_instruction(&BuildCreateTokenParams {
            user: &user,
            payer: None,
            coin_creator: &curve.coin_creator,
            base_mint: &curve.base_mint,
            quote_mint: &curve.quote_mint,
            partner: &curve.partner,
            platform_config: &curve.platform_config,
            quote_token_program: &address(&fixtures.account(&pubkey(&curve.quote_mint)).owner),
            name: "Send",
            symbol: "SEND",
            uri: "https://example.com/send.json",
            creator_fee_mode: curve.creator_fee_mode,
            creator_fee_bps: curve.creator_fee_bps,
            dex_creator_fee_bps: curve.dex_creator_fee_bps,
        });
        let parsed = ParsedInstruction {
            program_id: pubkey(&create.program_id),
            accounts: create
                .accounts
                .iter()
                .map(|meta| pubkey(&meta.pubkey))
                .collect(),
            data: create.data,
        };
        assert_eq!(
            parse_pool_creations(&[parsed]),
            vec![PoolCreation {
                protocol: PoolProtocol::SendfunLaunchpad,
                pool: market,
                mints: vec![pubkey(&curve.base_mint), pubkey(&curve.quote_mint)],
            }]
        );
    }

    // `create_pool`: the mints and the pool at 7, 8 and 9 of 22 accounts.
    let mut accounts: Vec<Pubkey> = (0..22).map(|_| Pubkey::new_unique()).collect();
    accounts[7] = POOL_WSOL_BASE;
    accounts[8] = WSOL;
    accounts[9] = POOL_WSOL;
    let create_pool = ParsedInstruction {
        program_id: DEX_PROGRAM_ID,
        accounts,
        data: vec![233, 146, 209, 142, 207, 104, 64, 188],
    };
    assert_eq!(
        parse_pool_creations(&[create_pool]),
        vec![PoolCreation {
            protocol: PoolProtocol::SendfunAmm,
            pool: POOL_WSOL,
            mints: vec![POOL_WSOL_BASE, WSOL],
        }]
    );
}
