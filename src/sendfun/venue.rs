//! [`SendfunVenue`]: one curve or pool as a Titan [`TradingVenue`].

use async_trait::async_trait;
use sendfun_sdk::constants::{
    ATA_PROGRAM_ID, NEXUS_PROGRAM_ID, SYSTEM_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use sendfun_sdk::math::amm::{AmmError, QuoteError};
use sendfun_sdk::nexus::accounts::PartnerConfig;
use sendfun_sdk::nexus::pda::find_partner_config_pda;
use sendfun_sdk::transfer_fee::{extension, mint_extensions};
use sendfun_sdk::transfer_hook::{
    AccountView, HookError, HookSwap, MarketHooks, MintState, Unloaded,
};
use sendfun_sdk::utils::{
    MarketQuoteError, TradeAccounts, TradeDirection, TradeMode, find_associated_token_pda,
    parse_token_amount,
};
use sendfun_sdk::{dex, launchpad};
use solana_account::Account;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_program::clock::Clock;
use solana_program::sysvar::clock;
use solana_pubkey::Pubkey;
use spl_token_2022_interface::extension::default_account_state::DefaultAccountState;
use spl_token_2022_interface::state::AccountState;

use super::bounds::{self, Limits, Range};
use super::convert::{to_address, to_pubkey};
use super::market::{MarketAccount, MarketKeys, MarketKind, Pricing, Snapshot, TransferFee};
use crate::account_caching::AccountsCache;
use crate::trading_venue::error::TradingVenueError;
use crate::trading_venue::protocol::PoolProtocol;
use crate::trading_venue::token_info::TokenInfo;
use crate::trading_venue::{FromAccount, QuoteRequest, QuoteResult, SwapType, TradingVenue};

/// A send.fun launchpad bonding curve or DEX pool.
///
/// - **Quotes** run [`sendfun_sdk`]'s quote of the market's program with the
///   venue's partner's fees and the market's creator fee rate, at the clock
///   of the last update. Each mint's transfer fee applies at that epoch.
///   `expected_output` is the amount that reaches the user's account. The
///   quote excludes the router's own transfers. [`TokenInfo`] gives each
///   mint's fee for them.
/// - **A buy past the supply left on a curve** fills the rest of the supply.
///   The quote sets `not_enough_liquidity`. `amount` is the input that the
///   fill spends.
/// - **Unroutable** (`initialized()` is `false`) when the curve is not
///   `Funding`, the pool is paused, the market account is closed, the
///   partner has no config on the market's platform, a mint is paused or
///   freezes new token accounts, or a mint's transfer hook does not resolve
///   before the swap. Also unroutable for one update when the cache refuses
///   an account that the update did not declare. The next update declares it.
#[derive(Clone, Debug)]
pub struct SendfunVenue {
    kind: MarketKind,
    market: Pubkey,
    keys: MarketKeys,
    /// The partner every trade routes as. A registered partner signs each
    /// trade. The default partner, the System Program key, does not. No
    /// partner is assumed: [`FromAccount::from_account`] reads the one set
    /// with [`set_partner`](super::set_partner).
    partner: Address,
    /// The partner's config on the market's platform.
    partner_config: Address,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    token_info: Vec<TokenInfo>,
    hooks: MarketHooks,
    clock: Option<ClockReading>,
    /// The time the fee decay premium reaches 0. `None` when unknown.
    decay_ends_at: Option<i64>,
    /// The latest epoch at which a mint's transfer fee changes.
    fee_change_epoch: Option<u64>,
    state: State,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClockReading {
    epoch: u64,
    unix_timestamp: i64,
}

#[derive(Clone, Debug)]
enum State {
    /// Before the first update.
    Unloaded,
    /// The market cannot trade. Holds the reason.
    Halted(&'static str),
    Tradable(Box<Tradable>),
}

#[derive(Clone, Debug)]
struct Tradable {
    snapshot: Snapshot,
    base_token_program: Address,
    quote_token_program: Address,
    /// `[buy, sell]`.
    ranges: [Range; 2],
    /// `[buy, sell]`: each direction's price at 0, with the transfer-fee rates
    /// at the start of its range.
    spots: [Result<f64, &'static str>; 2],
}

impl Tradable {
    const fn range(&self, direction: TradeDirection) -> Range {
        match direction {
            TradeDirection::Buy => self.ranges[0],
            TradeDirection::Sell => self.ranges[1],
        }
    }

    const fn spot(&self, direction: TradeDirection) -> Result<f64, &'static str> {
        match direction {
            TradeDirection::Buy => self.spots[0],
            TradeDirection::Sell => self.spots[1],
        }
    }
}

/// An account read during one update, with its owner as the SDK's type.
struct Read {
    account: Account,
    owner: Address,
}

impl Read {
    fn view(&self) -> AccountView<'_> {
        AccountView {
            owner: &self.owner,
            data: &self.account.data,
        }
    }
}

/// The accounts read during one update.
#[derive(Default)]
struct Fetched(Vec<(Pubkey, Option<Read>)>);

impl Fetched {
    /// Reads each of `keys` not read yet.
    async fn load(
        &mut self,
        cache: &dyn AccountsCache,
        keys: impl IntoIterator<Item = Pubkey>,
    ) -> Result<(), TradingVenueError> {
        let mut missing: Vec<Pubkey> = Vec::new();
        for key in keys {
            if !missing.contains(&key) && !self.0.iter().any(|(read, _)| *read == key) {
                missing.push(key);
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let accounts = cache.get_accounts(&missing).await?;
        if accounts.len() != missing.len() {
            return Err(TradingVenueError::FailedToFetchMultipleAccountData);
        }
        self.0
            .extend(missing.into_iter().zip(accounts).map(|(key, account)| {
                let read = account.map(|account| Read {
                    owner: to_address(&account.owner),
                    account,
                });
                (key, read)
            }));
        Ok(())
    }

    /// Reads each of `keys` not read yet, which the update did not declare.
    /// A cache may refuse them. Refused keys stay unread.
    async fn load_undeclared(
        &mut self,
        cache: &dyn AccountsCache,
        keys: impl IntoIterator<Item = Pubkey>,
    ) {
        let _ = self.load(cache, keys).await;
    }

    /// `true` once `key` is read, whether or not its account exists.
    fn was_read(&self, key: &Pubkey) -> bool {
        self.0.iter().any(|(read, _)| read == key)
    }

    fn get(&self, key: &Pubkey) -> Option<&Read> {
        self.0
            .iter()
            .find(|(read, _)| read == key)
            .and_then(|(_, read)| read.as_ref())
    }

    fn required(&self, key: &Pubkey) -> Result<&Read, TradingVenueError> {
        self.get(key)
            .ok_or(TradingVenueError::NoAccountFound((*key).into()))
    }
}

/// A mint as the venue reads it.
struct Mint {
    state: MintState,
    /// New token accounts start frozen.
    frozen_by_default: bool,
}

impl Mint {
    fn load(key: &Pubkey, read: &Read) -> Result<Self, TradingVenueError> {
        let unreadable = || TradingVenueError::DeserializationFailed((*key).into());
        let state = MintState::load(read.view()).map_err(|_| unreadable())?;
        let frozen_by_default = mint_extensions(&read.account.data, &read.owner)
            .and_then(|mint| {
                mint.map(|mint| extension::<DefaultAccountState>(&mint).map(Option::<&_>::copied))
                    .transpose()
            })
            .map_err(|_| unreadable())?
            .flatten()
            .is_some_and(|default| default.state == AccountState::Frozen as u8);
        Ok(Self {
            state,
            frozen_by_default,
        })
    }

    fn fee_change_epoch(&self) -> Option<u64> {
        self.state
            .transfer_fee
            .map(|config| u64::from(config.newer_transfer_fee.epoch))
    }
}

/// `true` if `info` carries the fee that `fee` charges.
fn fee_matches(info: &TokenInfo, fee: Option<sendfun_sdk::math::amm::MintFee>) -> bool {
    let expected = fee.map(|fee| (fee.bps, fee.maximum_fee));
    let carried = info.transfer_fee.zip(info.maximum_fee);
    expected == carried
}

const fn quote_error(error: MarketQuoteError) -> TradingVenueError {
    let reason = match error {
        MarketQuoteError::Quote(QuoteError::Amm(AmmError::InsufficientLiquidity)) => {
            "insufficient liquidity"
        }
        MarketQuoteError::Quote(QuoteError::Amm(AmmError::InvalidAmount)) => {
            "amount too small to trade"
        }
        MarketQuoteError::Quote(QuoteError::Amm(AmmError::Overflow)) => "arithmetic overflow",
        MarketQuoteError::Quote(QuoteError::InvalidTransferFee) => "invalid transfer fee",
        MarketQuoteError::Quote(QuoteError::TransferFeeNotSettleable) => {
            "transfer fee not settleable"
        }
        MarketQuoteError::FeeOutOfRange => "platform fee out of range",
        MarketQuoteError::SupplyExhausted => "bonding curve has no supply left",
        MarketQuoteError::ExceedsQuoteHeld => "sell exceeds the quote the curve holds",
        _ => "trade refused",
    };
    TradingVenueError::AmmMethodError(crate::trading_venue::error::ErrorInfo::StaticStr(reason))
}

impl SendfunVenue {
    fn new(kind: MarketKind, market: Pubkey, keys: MarketKeys, partner: Address) -> Self {
        Self {
            kind,
            market,
            keys,
            partner,
            partner_config: partner_config(&keys, &partner),
            base_mint: to_pubkey(&keys.base_mint),
            quote_mint: to_pubkey(&keys.quote_mint),
            token_info: Vec::new(),
            hooks: MarketHooks::default(),
            clock: None,
            decay_ends_at: None,
            fee_change_epoch: None,
            state: State::Unloaded,
        }
    }

    /// Builds the venue from a curve or pool account, to route as `partner`.
    /// A registered partner signs every trade. The System Program key names
    /// the default partner, which does not.
    pub fn from_account_with_partner(
        pubkey: &Pubkey,
        account: &Account,
        partner: &Pubkey,
    ) -> Result<Self, TradingVenueError> {
        let kind =
            MarketKind::of_owner(&account.owner).ok_or(TradingVenueError::FromAccountError(
                "account is not owned by the send.fun launchpad or DEX".into(),
            ))?;
        let market = MarketAccount::decode(kind, &account.data)
            .map_err(|reason| TradingVenueError::FromAccountError(reason.into()))?;
        Ok(Self::new(kind, *pubkey, market.keys, to_address(partner)))
    }

    /// The partner every trade routes as.
    #[must_use]
    pub const fn partner(&self) -> Pubkey {
        to_pubkey(&self.partner)
    }

    /// Which program the market belongs to.
    #[must_use]
    pub const fn kind(&self) -> MarketKind {
        self.kind
    }

    /// Why the market cannot trade. `None` once it can.
    #[must_use]
    pub const fn halted(&self) -> Option<&'static str> {
        match self.state {
            State::Unloaded => Some("market not updated"),
            State::Halted(reason) => Some(reason),
            State::Tradable(_) => None,
        }
    }

    fn direction(
        &self,
        input: &Pubkey,
        output: &Pubkey,
    ) -> Result<TradeDirection, TradingVenueError> {
        if *input == self.quote_mint && *output == self.base_mint {
            Ok(TradeDirection::Buy)
        } else if *input == self.base_mint && *output == self.quote_mint {
            Ok(TradeDirection::Sell)
        } else if *input == self.base_mint || *input == self.quote_mint {
            Err(TradingVenueError::InvalidMint((*output).into()))
        } else {
            Err(TradingVenueError::InvalidMint((*input).into()))
        }
    }

    fn tradable(&self) -> Result<&Tradable, TradingVenueError> {
        match &self.state {
            State::Tradable(tradable) => Ok(tradable),
            State::Unloaded => Err(TradingVenueError::NotInitialized(
                "market not updated".into(),
            )),
            State::Halted(_) => Err(TradingVenueError::InactivePoolError(
                self.market,
                self.protocol(),
            )),
        }
    }

    fn needs_clock(&self) -> bool {
        clock_moves_a_fee(self.clock, self.decay_ends_at, self.fee_change_epoch)
    }

    const fn fixed_keys(&self) -> [Pubkey; 6] {
        [
            self.market,
            self.base_mint,
            self.quote_mint,
            to_pubkey(&self.keys.base_vault),
            to_pubkey(&self.keys.quote_vault),
            to_pubkey(&self.partner_config),
        ]
    }

    /// The market's exact-in trade instruction for `trade`, with
    /// `min_amount_out` 0 and a read-only `payer`.
    ///
    /// The SDK marks `payer` writable, as the IDL does. The programs require
    /// a writable `payer` only to create a user token account. The trader's
    /// token accounts exist before the swap, so the trade takes no write lock
    /// on `payer`.
    fn trade_instruction(
        &self,
        trade: &TradeAccounts,
        direction: TradeDirection,
        amount: u64,
    ) -> solana_instruction_3::Instruction {
        let mut instruction = self.sdk_trade_instruction(trade, direction, amount);
        if let Some(payer) = instruction.accounts.get_mut(PAYER_INDEX) {
            payer.is_writable = false;
        }
        instruction
    }

    fn sdk_trade_instruction(
        &self,
        trade: &TradeAccounts,
        direction: TradeDirection,
        amount: u64,
    ) -> solana_instruction_3::Instruction {
        match self.kind {
            MarketKind::Launchpad => launchpad::trade::trade_instruction(
                trade,
                launchpad::trade::TradeArgs {
                    direction,
                    mode: TradeMode::ExactIn,
                    amount,
                    limit: 0,
                },
            ),
            MarketKind::Dex => dex::trade::trade_instruction(
                trade,
                dex::trade::TradeArgs {
                    direction: direction.into(),
                    mode: TradeMode::ExactIn,
                    amount,
                    limit: 0,
                },
            ),
        }
    }
}

/// `payer` in both programs' trade instructions: `user`, then `payer`.
const PAYER_INDEX: usize = 1;

/// `true` while the clock can change a fee: before the decay window closes,
/// or before a newer transfer fee takes effect. `true` with no reading.
fn clock_moves_a_fee(
    clock: Option<ClockReading>,
    decay_ends_at: Option<i64>,
    fee_change_epoch: Option<u64>,
) -> bool {
    let Some(clock) = clock else {
        return true;
    };
    let decaying = decay_ends_at.is_none_or(|ends_at| clock.unix_timestamp < ends_at);
    let fee_changing = fee_change_epoch.is_some_and(|epoch| epoch > clock.epoch);
    decaying || fee_changing
}

/// `partner`'s config on the market's platform.
fn partner_config(keys: &MarketKeys, partner: &Address) -> Address {
    find_partner_config_pda(&keys.platform_config, partner).0
}

/// The market's side of a trade, as `partner`. The trader's fields are
/// default.
fn market_side(
    market: &Pubkey,
    keys: &MarketKeys,
    partner: Address,
    partner_config: Address,
    quote_token_program: Address,
) -> TradeAccounts {
    TradeAccounts {
        market: to_address(market),
        base_mint: keys.base_mint,
        quote_mint: keys.quote_mint,
        base_vault: keys.base_vault,
        quote_vault: keys.quote_vault,
        partner,
        partner_config,
        quote_token_program,
        ..TradeAccounts::default()
    }
}

/// The quote of `direction` at `amount` against `snapshot`. `mints` are
/// `(input, output)`.
pub(super) fn quote_snapshot(
    snapshot: &Snapshot,
    direction: TradeDirection,
    amount: u64,
    mints: (Pubkey, Pubkey),
) -> Result<QuoteResult, TradingVenueError> {
    let (amount, expected_output, not_enough_liquidity) = if amount == 0 {
        (0, 0, false)
    } else {
        let quote = snapshot
            .market_quote(direction, amount)
            .map_err(quote_error)?;
        // The programs accept the input and pay nothing.
        if quote.out_amount == 0 {
            return Err(TradingVenueError::AmmMethodError(
                crate::trading_venue::error::ErrorInfo::StaticStr("swap delivers nothing"),
            ));
        }
        (quote.in_amount, quote.out_amount, quote.supply_capped)
    };
    let price = snapshot
        .price(direction, amount, expected_output)
        .map_err(|reason| TradingVenueError::MathError(reason.into()))?;
    Ok(QuoteResult {
        input_mint: mints.0,
        output_mint: mints.1,
        amount,
        expected_output,
        not_enough_liquidity,
        price,
    })
}

impl FromAccount for SendfunVenue {
    /// Routes as the partner set with [`set_partner`](super::set_partner).
    /// Fails when none is set: no partner is assumed.
    fn from_account(pubkey: &Pubkey, account: &Account) -> Result<Self, TradingVenueError> {
        let partner = super::partner().ok_or(TradingVenueError::FromAccountError(
            "no send.fun partner is set: call sendfun::set_partner first".into(),
        ))?;
        Self::from_account_with_partner(pubkey, account, &partner)
    }
}

#[async_trait]
impl TradingVenue for SendfunVenue {
    fn initialized(&self) -> bool {
        matches!(self.state, State::Tradable(_))
    }

    fn program_id(&self) -> Pubkey {
        self.kind.program_id()
    }

    fn program_dependencies(&self) -> Vec<Pubkey> {
        let mut programs = vec![self.kind.program_id()];
        programs.extend(
            [
                TOKEN_PROGRAM_ID,
                TOKEN_2022_PROGRAM_ID,
                ATA_PROGRAM_ID,
                SYSTEM_PROGRAM_ID,
            ]
            .iter()
            .chain(&self.hooks.programs())
            .map(to_pubkey),
        );
        programs
    }

    fn market_id(&self) -> Pubkey {
        self.market
    }

    /// The base mint, then the quote mint. Known from the market account,
    /// before the first update.
    fn tradable_mints(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        Ok(vec![self.base_mint, self.quote_mint])
    }

    /// The sell `(0, 1)`, then the buy `(1, 0)`. A routable venue declares a
    /// direction only if it has a range: a curve with no buys holds no quote,
    /// so it declares only the buy.
    fn directions_num(&self) -> Vec<(u8, u8)> {
        if self.token_info.len() < 2 {
            return Vec::new();
        }
        [
            (TradeDirection::Sell, (0, 1)),
            (TradeDirection::Buy, (1, 0)),
        ]
        .into_iter()
        .filter(|(direction, _)| match &self.state {
            State::Tradable(tradable) => tradable.range(*direction).is_ok(),
            State::Unloaded | State::Halted(_) => true,
        })
        .map(|(_, indices)| indices)
        .collect()
    }

    fn get_token_info(&self) -> &[TokenInfo] {
        &self.token_info
    }

    fn protocol(&self) -> PoolProtocol {
        self.kind.protocol()
    }

    /// The market, both mints, both vaults, the partner's config, each
    /// transfer hook's account list, and the clock while it can change a fee.
    fn get_required_pubkeys_for_update(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        let mut keys = self.fixed_keys().to_vec();
        keys.extend(
            self.hooks
                .validation_accounts()
                .map(|list| to_pubkey(&list)),
        );
        if self.needs_clock() {
            keys.push(clock::ID);
        }
        Ok(keys)
    }

    /// All or nothing. On a cache error, the venue keeps its last state. An
    /// account that does not decode halts the venue until an update decodes
    /// it: the error repeats, so the last state would only go stale.
    async fn update_state(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let updated = self.update(cache).await;
        if let Err(TradingVenueError::DeserializationFailed(_)) = updated {
            self.state = State::Halted("an account does not decode");
        }
        updated
    }

    /// Exact-in only. An `amount` of 0 returns no output at the spot price.
    fn quote(&self, request: QuoteRequest) -> Result<QuoteResult, TradingVenueError> {
        if request.swap_type == SwapType::ExactOut {
            return Err(TradingVenueError::ExactOutNotSupported);
        }
        let direction = self.direction(&request.input_mint, &request.output_mint)?;
        let tradable = self.tradable()?;
        if request.amount == 0 {
            let price = tradable
                .spot(direction)
                .map_err(|reason| TradingVenueError::MathError(reason.into()))?;
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
                price,
            });
        }
        quote_snapshot(
            &tradable.snapshot,
            direction,
            request.amount,
            (request.input_mint, request.output_mint),
        )
    }

    /// `buy_exact_in` or `sell_exact_in` of `request.amount` with
    /// `min_amount_out` 0, as the venue's partner. `user` signs as the trader
    /// and as a read-only `payer`, and owns its ATAs of both mints. Both ATAs
    /// must exist. A registered partner is a read-only signer: the
    /// transaction carries its signature. The default partner does not sign.
    /// Transfer-hook accounts follow the program's accounts.
    fn generate_swap_instruction(
        &self,
        request: QuoteRequest,
        user: Pubkey,
    ) -> Result<Instruction, TradingVenueError> {
        if request.swap_type == SwapType::ExactOut {
            return Err(TradingVenueError::ExactOutNotSupported);
        }
        let direction = self.direction(&request.input_mint, &request.output_mint)?;
        let tradable = self.tradable()?;
        let owner = to_address(&user);
        let trade = TradeAccounts {
            user: owner,
            payer: owner,
            user_base_account: find_associated_token_pda(
                &owner,
                &self.keys.base_mint,
                &tradable.base_token_program,
            )
            .0,
            user_quote_account: find_associated_token_pda(
                &owner,
                &self.keys.quote_mint,
                &tradable.quote_token_program,
            )
            .0,
            ..market_side(
                &self.market,
                &self.keys,
                self.partner,
                self.partner_config,
                tradable.quote_token_program,
            )
        };
        let instruction = self.trade_instruction(&trade, direction, request.amount);
        let hook_accounts = self
            .hooks
            .swap_accounts(
                HookSwap {
                    trade: &trade,
                    direction,
                    fixed: &instruction.accounts,
                },
                Unloaded::Refuse,
            )
            .map_err(|_| {
                TradingVenueError::AmmMethodError(
                    "transfer hook accounts do not resolve for this user".into(),
                )
            })?;
        Ok(Instruction {
            program_id: self.kind.program_id(),
            accounts: instruction
                .accounts
                .iter()
                .chain(&hook_accounts)
                .map(|meta| AccountMeta {
                    pubkey: to_pubkey(&meta.pubkey),
                    is_signer: meta.is_signer,
                    is_writable: meta.is_writable,
                })
                .collect(),
            data: instruction.data,
        })
    }

    /// The range certified at the last update. Does not allocate.
    fn bounds(&self, tkn_in_ind: u8, tkn_out_ind: u8) -> Result<(u64, u64), TradingVenueError> {
        let input = self.get_token(usize::from(tkn_in_ind))?.pubkey;
        let output = self.get_token(usize::from(tkn_out_ind))?.pubkey;
        let direction = self.direction(&input, &output)?;
        self.tradable()?
            .range(direction)
            .map_err(|reason| TradingVenueError::NoQuotableValue(reason.into()))
    }
}

impl SendfunVenue {
    /// Reads every account and rebuilds the state. Changes nothing on an
    /// error.
    async fn update(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let mut fetched = Fetched::default();
        let clock_requested = self.needs_clock();
        // A cache error on any declared account fails the update.
        let declared = self.get_required_pubkeys_for_update()?;
        fetched.load(cache, declared).await?;

        let Some(market) = fetched
            .get(&self.market)
            .filter(|read| read.account.owner == self.kind.program_id())
        else {
            self.state = State::Halted("market account is closed");
            return Ok(());
        };
        let market = MarketAccount::decode(self.kind, &market.account.data)
            .map_err(|reason| TradingVenueError::DeserializationFailed(reason.into()))?;
        // A relaunched curve can name another platform.
        let keys = market.keys;
        let partner_config = partner_config(&keys, &self.partner);
        fetched
            .load_undeclared(
                cache,
                [
                    keys.base_mint,
                    keys.quote_mint,
                    keys.base_vault,
                    keys.quote_vault,
                    partner_config,
                ]
                .iter()
                .map(to_pubkey),
            )
            .await;

        let (base_mint, quote_mint) = (to_pubkey(&keys.base_mint), to_pubkey(&keys.quote_mint));
        let base = Mint::load(&base_mint, fetched.required(&base_mint)?)?;
        let quote = Mint::load(&quote_mint, fetched.required(&quote_mint)?)?;
        let vault_amount = |vault: &Address| {
            let vault = to_pubkey(vault);
            parse_token_amount(&fetched.required(&vault)?.account.data)
                .ok_or(TradingVenueError::DeserializationFailed(vault.into()))
        };
        let base_vault_amount = vault_amount(&keys.base_vault)?;
        let quote_vault_amount = vault_amount(&keys.quote_vault)?;
        // Anyone can fund the address. Only nexus writes a config. The
        // programs also refuse a config that names another partner or
        // platform.
        let partner_key = self.partner;
        let partner = fetched
            .get(&to_pubkey(&partner_config))
            .filter(|read| read.owner == NEXUS_PROGRAM_ID)
            .map(|read| PartnerConfig::from_bytes(&read.account.data))
            .transpose()
            .map_err(|_| {
                TradingVenueError::DeserializationFailed(to_pubkey(&partner_config).into())
            })?
            .filter(|config| {
                config.partner == partner_key && config.platform_config == keys.platform_config
            });

        // The mints name the hook lists. A list the update did not declare
        // loads in a second read.
        let plan = self.hooks.plan(
            (&keys.base_mint, &base.state),
            (&keys.quote_mint, &quote.state),
        );
        let lists: Vec<Pubkey> = plan.accounts().map(|list| to_pubkey(&list)).collect();
        let (base_state, quote_state) = (base.state, quote.state);
        let frozen = base.frozen_by_default || quote.frozen_by_default;
        let fee_change_epoch = base.fee_change_epoch().max(quote.fee_change_epoch());
        fetched.load_undeclared(cache, lists).await;

        let pricing = partner
            .as_ref()
            .map(|partner| Pricing::new(&market, partner, base_vault_amount));
        let decay_ends_at = pricing.as_ref().and_then(|pricing| {
            market
                .created_at
                .checked_add(i64::from(pricing.decay_seconds()))
        });
        // A new curve or a new transfer fee can need a clock that this update
        // did not declare.
        let clock_due =
            !clock_requested && clock_moves_a_fee(self.clock, decay_ends_at, fee_change_epoch);
        if clock_due {
            fetched.load_undeclared(cache, [clock::ID]).await;
        }
        let fresh_clock = fetched
            .get(&clock::ID)
            .map(|read| {
                bincode::deserialize::<Clock>(&read.account.data)
                    .map(|clock| ClockReading {
                        epoch: clock.epoch,
                        unix_timestamp: clock.unix_timestamp,
                    })
                    .map_err(|_| TradingVenueError::DeserializationFailed(clock::ID.into()))
            })
            .transpose()?;
        // A missing requested clock fails the update. A missing undeclared
        // clock halts the market until the next update.
        let clock_unread = clock_due && fresh_clock.is_none();
        let clock = fresh_clock
            .or_else(|| self.clock.filter(|_| !clock_requested))
            .ok_or(TradingVenueError::NoAccountFound(clock::ID.into()))?;
        let base_info = TokenInfo::new(
            &base_mint,
            &fetched.required(&base_mint)?.account,
            clock.epoch,
        )?;
        let quote_info = TokenInfo::new(
            &quote_mint,
            &fetched.required(&quote_mint)?.account,
            clock.epoch,
        )?;

        let resolved = plan.resolve(|list| fetched.get(&to_pubkey(list)).map(Read::view));
        let quote_token_program = quote_state.token_program;
        let stand_in = market_side(
            &self.market,
            &keys,
            self.partner,
            partner_config,
            quote_token_program,
        )
        .with_stand_in_trader();
        let fixed = self
            .trade_instruction(&stand_in, TradeDirection::Buy, 0)
            .accounts;
        let hooks = resolved.screen(&stand_in, &fixed);

        let landing = sendfun_sdk::utils::Landing {
            base_fee: base_state.fee_at(clock.epoch),
            quote_fee: quote_state.fee_at(clock.epoch),
            unix_timestamp: clock.unix_timestamp,
        };
        let fee_bps = pricing
            .as_ref()
            .and_then(|pricing| pricing.fee_bps(market.created_at, clock.unix_timestamp));
        let halted = market
            .halted
            .or_else(|| {
                (!fetched.was_read(&to_pubkey(&partner_config)))
                    .then_some("partner config not loaded")
            })
            .or_else(|| clock_unread.then_some("clock not loaded"))
            .or_else(|| {
                partner
                    .is_none()
                    .then_some("the partner has no config on this market's platform")
            })
            .or_else(|| (base_state.paused || quote_state.paused).then_some("a mint is paused"))
            .or_else(|| frozen.then_some("a mint freezes new token accounts"))
            .or_else(|| match hooks.check_routable(Unloaded::Refuse) {
                Ok(()) => None,
                Err(HookError::NotLoaded) => Some("transfer hook accounts not loaded"),
                Err(_) => Some("a transfer hook cannot be resolved before the swap"),
            })
            .or_else(|| {
                (!fee_matches(&base_info, landing.base_fee)
                    || !fee_matches(&quote_info, landing.quote_fee))
                .then_some("token info disagrees with a mint's transfer fee")
            })
            .or_else(|| fee_bps.is_none().then_some("platform fee out of range"));

        let state = match (halted, pricing, fee_bps) {
            (None, Some(pricing), Some(fee_bps)) => {
                let snapshot = Snapshot {
                    pricing,
                    created_at: market.created_at,
                    landing,
                    fee_bps,
                    base_fee: TransferFee::new(landing.base_fee),
                    quote_fee: TransferFee::new(landing.quote_fee),
                };
                // A direction's range, and its price at 0.
                let route = |direction: TradeDirection| {
                    let (input_vault_amount, mints) = match direction {
                        TradeDirection::Buy => (quote_vault_amount, (quote_mint, base_mint)),
                        TradeDirection::Sell => (base_vault_amount, (base_mint, quote_mint)),
                    };
                    let (input_fee, output_fee) = snapshot.transfer_fees(direction);
                    let quoter = |amount: u64| quote_snapshot(&snapshot, direction, amount, mints);
                    let range = u64::MAX
                        .checked_sub(input_vault_amount)
                        .ok_or("vault overflow")
                        .and_then(|max_input| {
                            bounds::range(&Limits {
                                max_input,
                                input_cap: input_fee.cap(),
                                output_at_cap: output_fee.received_at_cap(),
                                quoter: &quoter,
                            })
                        });
                    let spot = match range {
                        Ok((lower, _)) => quoter(lower)
                            .map_err(|_| "the lower bound does not quote")
                            .and_then(|quote| {
                                snapshot.spot_price(direction, lower, quote.expected_output)
                            }),
                        Err(_) => snapshot.price(direction, 0, 0),
                    };
                    (range, spot)
                };
                let routes = [route(TradeDirection::Buy), route(TradeDirection::Sell)];
                State::Tradable(Box::new(Tradable {
                    snapshot,
                    base_token_program: base_state.token_program,
                    quote_token_program,
                    ranges: routes.map(|(range, _)| range),
                    spots: routes.map(|(_, spot)| spot),
                }))
            }
            (halted, ..) => State::Halted(halted.unwrap_or("platform fee out of range")),
        };

        self.keys = keys;
        self.partner_config = partner_config;
        self.base_mint = base_mint;
        self.quote_mint = quote_mint;
        self.token_info = vec![base_info, quote_info];
        self.hooks = hooks;
        self.clock = Some(clock);
        self.decay_ends_at = decay_ends_at;
        self.fee_change_epoch = fee_change_epoch;
        self.state = state;
        Ok(())
    }
}
