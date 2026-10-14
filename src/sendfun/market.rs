//! Market accounts and the state a quote reads.

use sendfun_sdk::dex::accounts::Pool;
use sendfun_sdk::dex::types::PoolStatus;
use sendfun_sdk::dex::{self, PoolMarket};
use sendfun_sdk::launchpad::accounts::BondingCurve;
use sendfun_sdk::launchpad::types::BondingCurveStatus;
use sendfun_sdk::launchpad::{self, CurveMarket};
use sendfun_sdk::math::amm::{MintFee, fee_on};
use sendfun_sdk::nexus::EffectiveFeeArgs;
use sendfun_sdk::nexus::accounts::PartnerConfig;
use sendfun_sdk::nexus::types::{DexFees, LaunchpadFees};
use sendfun_sdk::utils::{
    Landing, MarketQuote, MarketQuoteError, QuoteRequest, TradeDirection, TradeMode,
};
use solana_address::Address;
use solana_pubkey::Pubkey;

use super::{DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID};
use crate::trading_venue::protocol::PoolProtocol;

const BPS: f64 = 10_000.0;

/// The program that a market belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketKind {
    /// A launchpad bonding curve. It trades while `Funding`.
    Launchpad,
    /// A DEX pool. It trades while `Active`.
    Dex,
}

impl MarketKind {
    #[must_use]
    pub const fn program_id(self) -> Pubkey {
        match self {
            Self::Launchpad => LAUNCHPAD_PROGRAM_ID,
            Self::Dex => DEX_PROGRAM_ID,
        }
    }

    #[must_use]
    pub const fn protocol(self) -> PoolProtocol {
        match self {
            Self::Launchpad => PoolProtocol::SendfunLaunchpad,
            Self::Dex => PoolProtocol::SendfunAmm,
        }
    }

    #[must_use]
    pub fn of_owner(owner: &Pubkey) -> Option<Self> {
        if *owner == LAUNCHPAD_PROGRAM_ID {
            Some(Self::Launchpad)
        } else if *owner == DEX_PROGRAM_ID {
            Some(Self::Dex)
        } else {
            None
        }
    }
}

/// The accounts a market account names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketKeys {
    pub base_mint: Address,
    pub quote_mint: Address,
    pub base_vault: Address,
    pub quote_vault: Address,
    /// The market's platform. It seeds the partner config PDA.
    pub platform_config: Address,
}

/// The reserves a market account holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserves {
    /// The curve prices off its virtual reserves. A buy fills at most
    /// `real_base`. A sell pays out at most `real_quote`, fee included.
    Curve {
        virtual_base: u64,
        virtual_quote: u64,
        real_base: u64,
        real_quote: u64,
    },
    /// The pool prices its quote side off `quote_reserves`, and its base side
    /// off the base vault's balance.
    Pool { quote_reserves: u64 },
}

/// A decoded curve or pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketAccount {
    pub keys: MarketKeys,
    pub reserves: Reserves,
    pub created_at: i64,
    /// The creator fee rate that each trade on the market pays, in bps.
    pub creator_fee_bps: u16,
    /// The reason the status stops trading. `None` while the market trades.
    pub halted: Option<&'static str>,
}

impl MarketAccount {
    /// Decodes `data` and checks its discriminator. Does not check the owner.
    pub fn decode(kind: MarketKind, data: &[u8]) -> Result<Self, &'static str> {
        match kind {
            MarketKind::Launchpad => {
                let curve = BondingCurve::from_bytes(data)
                    .map_err(|_| "account is not a send.fun bonding curve")?;
                Ok(Self {
                    keys: MarketKeys {
                        base_mint: curve.base_mint,
                        quote_mint: curve.quote_mint,
                        base_vault: curve.base_vault,
                        quote_vault: curve.quote_vault,
                        platform_config: curve.platform_config,
                    },
                    reserves: Reserves::Curve {
                        virtual_base: curve.virtual_base_reserves,
                        virtual_quote: curve.virtual_quote_reserves,
                        real_base: curve.real_base_reserves,
                        real_quote: curve.real_quote_reserves,
                    },
                    created_at: curve.created_at,
                    creator_fee_bps: curve.creator_fee_bps,
                    halted: match curve.status {
                        BondingCurveStatus::Funding => None,
                        BondingCurveStatus::Completed => Some("bonding curve is complete"),
                        BondingCurveStatus::Migrated => Some("bonding curve has migrated"),
                        BondingCurveStatus::Paused => Some("bonding curve is paused"),
                    },
                })
            }
            MarketKind::Dex => {
                let pool =
                    Pool::from_bytes(data).map_err(|_| "account is not a send.fun DEX pool")?;
                Ok(Self {
                    keys: MarketKeys {
                        base_mint: pool.base_mint,
                        quote_mint: pool.quote_mint,
                        base_vault: pool.base_vault,
                        quote_vault: pool.quote_vault,
                        platform_config: pool.platform_config,
                    },
                    reserves: Reserves::Pool {
                        quote_reserves: pool.quote_reserves,
                    },
                    created_at: pool.created_at,
                    creator_fee_bps: pool.creator_fee_bps,
                    halted: match pool.status {
                        PoolStatus::Active => None,
                        PoolStatus::Paused => Some("pool is paused"),
                    },
                })
            }
        }
    }
}

/// A market's reserves and creator fee rate, with the partner's fee schedule
/// for its program.
///
/// The creator fee rate is the market's own. The partner's
/// `max_creator_fee_bps` limits it at the market's creation and is not part of
/// a trade's fee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pricing {
    Curve {
        virtual_base: u64,
        virtual_quote: u64,
        real_base: u64,
        real_quote: u64,
        /// `BondingCurve.creator_fee_bps`.
        creator_fee_bps: u16,
        fees: LaunchpadFees,
    },
    Pool {
        base_vault_amount: u64,
        quote_reserves: u64,
        /// `Pool.creator_fee_bps`.
        creator_fee_bps: u16,
        fees: DexFees,
    },
}

impl Pricing {
    /// A pool prices off `base_vault_amount`. A curve ignores it.
    pub fn new(market: &MarketAccount, partner: &PartnerConfig, base_vault_amount: u64) -> Self {
        let creator_fee_bps = market.creator_fee_bps;
        match market.reserves {
            Reserves::Curve {
                virtual_base,
                virtual_quote,
                real_base,
                real_quote,
            } => Self::Curve {
                virtual_base,
                virtual_quote,
                real_base,
                real_quote,
                creator_fee_bps,
                fees: partner.launchpad.clone(),
            },
            Reserves::Pool { quote_reserves } => Self::Pool {
                base_vault_amount,
                quote_reserves,
                creator_fee_bps,
                fees: partner.dex.clone(),
            },
        }
    }

    /// The platform fee rate at `now`, in bps: the partner's rates, the
    /// market's creator fee rate and the decay premium. `None` when the rate is
    /// out of range.
    pub fn fee_bps(&self, created_at: i64, now: i64) -> Option<u16> {
        let args = |creator_fee_bps| EffectiveFeeArgs {
            creator_fee_bps,
            created_at,
            now,
        };
        match self {
            Self::Curve {
                creator_fee_bps,
                fees,
                ..
            } => fees.effective_fee_bps(args(*creator_fee_bps)),
            Self::Pool {
                creator_fee_bps,
                fees,
                ..
            } => fees.effective_fee_bps(args(*creator_fee_bps)),
        }
    }

    /// The decay window, in seconds. The decay premium is 0 from
    /// `created_at + fee_decay_seconds`.
    pub const fn decay_seconds(&self) -> u16 {
        match self {
            Self::Curve { fees, .. } => fees.fee_decay_seconds,
            Self::Pool { fees, .. } => fees.fee_decay_seconds,
        }
    }

    /// `(quote, base)`: the reserves the constant product prices off.
    const fn reserves(&self) -> (u64, u64) {
        match *self {
            Self::Curve {
                virtual_base,
                virtual_quote,
                ..
            } => (virtual_quote, virtual_base),
            Self::Pool {
                base_vault_amount,
                quote_reserves,
                ..
            } => (quote_reserves, base_vault_amount),
        }
    }
}

/// A mint's transfer fee at the landing epoch, as the price reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransferFee {
    pub fee: Option<MintFee>,
    /// The fee rate as a fraction of the transfer, below `maximum_fee`.
    rate: f64,
    /// The smallest transfer whose fee reaches `maximum_fee`. The fee is flat
    /// from there. `None` if no transfer reaches it or the fee is always 0.
    cap: Option<u64>,
}

impl TransferFee {
    pub fn new(fee: Option<MintFee>) -> Self {
        let charged = fee.filter(|fee| fee.bps > 0 && fee.maximum_fee > 0);
        Self {
            fee,
            rate: charged.map_or(0.0, |fee| f64::from(fee.bps) / BPS),
            cap: charged.and_then(cap_point),
        }
    }

    /// The first transfer that pays `maximum_fee`.
    pub const fn cap(&self) -> Option<u64> {
        self.cap
    }

    /// The amount a transfer of `cap` lands. A capped and an uncapped transfer
    /// can both land it. A smaller landed amount is uncapped. A larger one is
    /// capped.
    pub fn received_at_cap(&self) -> Option<u64> {
        self.cap?.checked_sub(self.fee?.maximum_fee)
    }

    /// The marginal fee rate on a transfer of `amount`.
    fn rate_in(&self, amount: u64) -> f64 {
        if self.cap.is_some_and(|cap| amount >= cap) {
            0.0
        } else {
            self.rate
        }
    }

    /// The marginal fee rate on the transfer that lands `received`.
    fn rate_out(&self, received: u64) -> f64 {
        if self
            .received_at_cap()
            .is_some_and(|at_cap| received > at_cap)
        {
            0.0
        } else {
            self.rate
        }
    }
}

/// The smallest transfer whose fee reaches `maximum_fee`.
fn cap_point(fee: MintFee) -> Option<u64> {
    let reaches = |amount: u64| fee_on(amount, Some(fee)).is_ok_and(|paid| paid >= fee.maximum_fee);
    // The search needs `fee_on` to not decrease as the amount grows.
    if !reaches(u64::MAX) {
        return None;
    }
    let (mut below, mut at) = (0_u64, u64::MAX);
    while let Some(gap) = at.checked_sub(below).filter(|gap| *gap > 1) {
        let mid = below.checked_add(gap / 2)?;
        if reaches(mid) {
            at = mid;
        } else {
            below = mid;
        }
    }
    Some(at)
}

/// What a quote reads, fixed at one update.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub pricing: Pricing,
    pub created_at: i64,
    /// The transfer fees and the landing time.
    pub landing: Landing,
    /// The platform fee at `landing.unix_timestamp`.
    pub fee_bps: u16,
    pub base_fee: TransferFee,
    pub quote_fee: TransferFee,
}

impl Snapshot {
    /// `(input, output)` transfer fees of a trade.
    pub const fn transfer_fees(&self, direction: TradeDirection) -> (&TransferFee, &TransferFee) {
        match direction {
            TradeDirection::Buy => (&self.quote_fee, &self.base_fee),
            TradeDirection::Sell => (&self.base_fee, &self.quote_fee),
        }
    }

    /// The SDK's exact-in quote. Fails on an `amount` of 0.
    pub fn market_quote(
        &self,
        direction: TradeDirection,
        amount: u64,
    ) -> Result<MarketQuote, MarketQuoteError> {
        let request = QuoteRequest {
            direction,
            mode: TradeMode::ExactIn,
            amount,
        };
        match &self.pricing {
            Pricing::Curve {
                virtual_base,
                virtual_quote,
                real_base,
                real_quote,
                creator_fee_bps,
                fees,
            } => launchpad::quote(
                &CurveMarket {
                    virtual_base_reserves: *virtual_base,
                    virtual_quote_reserves: *virtual_quote,
                    real_base_reserves: *real_base,
                    real_quote_reserves: *real_quote,
                    created_at: self.created_at,
                    fees,
                    landing: self.landing,
                    creator_fee_bps: *creator_fee_bps,
                },
                request,
            ),
            Pricing::Pool {
                base_vault_amount,
                quote_reserves,
                creator_fee_bps,
                fees,
            } => dex::quote(
                &PoolMarket {
                    base_vault_amount: *base_vault_amount,
                    quote_reserves: *quote_reserves,
                    created_at: self.created_at,
                    fees,
                    landing: self.landing,
                    creator_fee_bps: *creator_fee_bps,
                },
                request,
            ),
        }
    }

    /// `d(out)/d(in)` of the exact-in quote at `amount`, in raw atoms.
    /// `received` is the quote's output at `amount`.
    ///
    /// With `t` a transfer-fee rate (0 once the transfer pays the maximum
    /// fee), `b` the platform fee rate, and `Rq`, `Rb` the pricing reserves:
    ///
    /// ```text
    /// buy:  n = (x - fee_q(x))(1 - b)
    ///       price = (1 - t_q)(1 - b) Rq Rb / (Rq + n)^2 (1 - t_b)
    /// sell: i = x - fee_b(x)
    ///       price = (1 - t_b)(1 - b) Rq Rb / (Rb + i)^2 (1 - t_q)
    /// ```
    pub fn price(
        &self,
        direction: TradeDirection,
        amount: u64,
        received: u64,
    ) -> Result<f64, &'static str> {
        self.price_at_rates(direction, amount, (amount, received))
    }

    /// The spot price of a range that starts at `lower`, which lands
    /// `received`: the price at 0, with each transfer fee's rate at `lower`.
    /// Past a cap point that rate is 0.
    pub fn spot_price(
        &self,
        direction: TradeDirection,
        lower: u64,
        received: u64,
    ) -> Result<f64, &'static str> {
        self.price_at_rates(direction, 0, (lower, received))
    }

    /// [`Self::price`] at `amount`, with the transfer fees' rates of the
    /// transfer of `rates_at.0` that lands `rates_at.1`.
    fn price_at_rates(
        &self,
        direction: TradeDirection,
        amount: u64,
        rates_at: (u64, u64),
    ) -> Result<f64, &'static str> {
        let (input, output) = self.transfer_fees(direction);
        let landed = fee_on(amount, input.fee)
            .ok()
            .and_then(|fee| amount.checked_sub(fee))
            .ok_or("transfer fee exceeds the transfer")?;
        let keep = 1.0 - f64::from(self.fee_bps) / BPS;
        let (quote_reserves, base_reserves) = self.pricing.reserves();
        let (rq, rb) = (quote_reserves as f64, base_reserves as f64);
        let denominator = match direction {
            TradeDirection::Buy => (landed as f64).mul_add(keep, rq),
            TradeDirection::Sell => rb + landed as f64,
        };
        let (rate_amount, rate_received) = rates_at;
        Ok(
            (1.0 - input.rate_in(rate_amount)) * keep * rq * rb / (denominator * denominator)
                * (1.0 - output.rate_out(rate_received)),
        )
    }
}
