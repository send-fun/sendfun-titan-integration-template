//! send.fun as a Titan venue: launchpad bonding curves and DEX pools.
//!
//! [`SendfunVenue`] serves both kinds of market. Build it from the curve or
//! pool account:
//!
//! | Market | Owner | Protocol |
//! | --- | --- | --- |
//! | bonding curve | [`LAUNCHPAD_PROGRAM_ID`] | `send.fun Launchpad` |
//! | DEX pool | [`DEX_PROGRAM_ID`] | `send.fun AMM` |
//!
//! Token 0 is the base mint. Token 1 is the quote mint. A buy spends quote and
//! receives base. A sell spends base and receives quote.
//!
//! Every trade routes as one send.fun partner. A registered partner is a key
//! that send.fun registers on each platform, with its own fee schedule. It
//! signs every trade. The default partner is the System Program key. It signs
//! nothing and pays the unpartnered schedule. No partner is assumed. Call
//! [`set_partner`] once, before the first
//! [`FromAccount::from_account`](crate::trading_venue::FromAccount::from_account),
//! or build each venue with [`SendfunVenue::from_account_with_partner`].

#![warn(clippy::all, clippy::pedantic, clippy::perf, clippy::nursery)]
#![allow(
    missing_docs,
    clippy::cognitive_complexity,
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the lint set this venue is held to"
)]
#![allow(
    clippy::result_large_err,
    reason = "`TradingVenueError` is the template's error type"
)]
#![deny(
    unfulfilled_lint_expectations,
    clippy::await_holding_lock,
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::print_stdout,
    clippy::dbg_macro,
    clippy::mem_forget
)]
#![warn(
    clippy::mod_module_files,
    clippy::undocumented_unsafe_blocks,
    clippy::indexing_slicing,
    clippy::clone_on_ref_ptr,
    clippy::arithmetic_side_effects
)]

mod bounds;
mod convert;
mod creation;
mod market;
mod venue;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use solana_pubkey::Pubkey;

pub use creation::parse_pool_creations;
pub use market::MarketKind;
pub use venue::SendfunVenue;

/// The launchpad: token creation and bonding-curve trading.
pub const LAUNCHPAD_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("5R1uFyEE4oxqkm7hJDqHq6gXaLVF9dxeF3LF4yz1sfLP");

/// The DEX: constant-product pools.
pub const DEX_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("84qj5FPZZdXkQy8mfowyg6RBZ3XKuTds6XS4ZYT1sfDX");

static PARTNER: OnceLock<Pubkey> = OnceLock::new();

/// Sets the partner that `from_account` routes as.
///
/// Call it once, at startup, before the first
/// [`FromAccount::from_account`](crate::trading_venue::FromAccount::from_account).
/// The System Program key names the default partner. Fails with the partner
/// already set when that is another key.
pub fn set_partner(partner: Pubkey) -> Result<(), Pubkey> {
    let set = *PARTNER.get_or_init(|| partner);
    if set == partner { Ok(()) } else { Err(set) }
}

/// The partner set with [`set_partner`]. `None` until it is called.
#[must_use]
pub fn partner() -> Option<Pubkey> {
    PARTNER.get().copied()
}

/// The smallest lower bound [`SendfunVenue`] reports. See
/// [`SendfunVenue::bounds`](crate::trading_venue::TradingVenue::bounds).
pub const MIN_LOWER_BOUND: u64 = 1_000_000;
