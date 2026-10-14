//! Titan's shared venue suite on every send.fun test market, routed as the
//! default partner: the System Program key. It signs nothing and pays each
//! platform's unpartnered schedule. The venue routes as it only when it is
//! named, as here.
//!
//! The tests skip without `SOLANA_RPC_URL`. The simulations also skip without
//! the dumped program binaries.

mod common;

use common::SuiteConfig;
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::sendfun::{
    DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, SendfunVenue, set_partner,
};

// Installs the allocation guard that powers the construction test's
// `assert_no_alloc` checks. The Makefile runs that test under `release-debug`
// so the guard is active; speed tests run under true `--release`.
#[cfg(debug_assertions)]
#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const DEFAULT_PARTNER: Pubkey = pubkey!("11111111111111111111111111111111");

fn config(pool: Pubkey, program: Pubkey) -> SuiteConfig {
    set_partner(DEFAULT_PARTNER).unwrap();
    SuiteConfig {
        pool,
        programs: vec![program],
    }
}

/// The shared suite on one market.
macro_rules! suite {
    ($market:ident, $pool:literal, $program:expr) => {
        mod $market {
            use super::*;

            fn config() -> SuiteConfig {
                super::config(pubkey!($pool), $program)
            }

            #[tokio::test]
            async fn construction() {
                common::construction::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn zero_input_spot_price() {
                common::zero_input_spot_price::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn bound_simulation() {
                common::bound_simulation::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn random_samples() {
                common::random_samples::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn monotone() {
                common::monotone::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn quoting_speed() {
                common::quoting_speed::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn price_monotone() {
                common::price_monotone::<SendfunVenue>(&config()).await;
            }

            #[tokio::test]
            async fn mean_value_theorem() {
                common::mean_value_theorem::<SendfunVenue>(&config()).await;
            }
        }
    };
}

suite!(
    curve_wsol,
    "Eq52yham9BNMbz2pCcqpSFUNAfJZKJFPNEhqHaEdyy7L",
    LAUNCHPAD_PROGRAM_ID
);
suite!(
    curve_fee,
    "ECxRrNU6Ggd73c3AD7UPd6ijfJYdULWFT2jDJZu5gzcd",
    LAUNCHPAD_PROGRAM_ID
);
suite!(
    dex_pool,
    "ChJu4A7Gow1DDsWdgHvcpJtxDQQZvA5tavWKn8zZ9kY5",
    DEX_PROGRAM_ID
);
// On hostile.xyz's platform.
suite!(
    hostile_curve,
    "DgH8nF1MiQrTiCeNF6fYfAsNhEhqawN4Q9JNuAt2UoM6",
    LAUNCHPAD_PROGRAM_ID
);
suite!(
    hostile_pool,
    "nBB3wTa7qDsv2Pkrwg9zoNfWvcjtQ9J2MmdTPhnVW1Y",
    DEX_PROGRAM_ID
);
