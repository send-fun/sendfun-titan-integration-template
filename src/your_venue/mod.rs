//! The integration's venue: send.fun, from [`crate::sendfun`].
//!
//! Build a [`SendfunVenue`] from a launchpad bonding curve or a DEX pool
//! account. [`parse_pool_creations`] finds new curves and pools.

pub use crate::sendfun::{
    DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, MarketKind, SendfunVenue, parse_pool_creations,
    set_partner,
};
