//! Titan's shared venue suite on a bonding curve and a DEX pool of
//! hostile.xyz's platform, `6fBeMZFvaq4sNXNGNpKW1wNZgN42v8M3NxA4bCrHNjam`. A
//! market names its platform, and a partner has one config per platform.
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

/// send.fun's own partner key, registered on every platform. The simulations
/// route as it. Titan routes as the key that send.fun registers for Titan.
const PARTNER: Pubkey = pubkey!("1TZSDDRKxjgEFatTYAbSgM8kxZbSAjTRPZxHYd7send");

fn config(pool: Pubkey, program: Pubkey) -> SuiteConfig {
    set_partner(PARTNER).unwrap();
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
    curve,
    "DgH8nF1MiQrTiCeNF6fYfAsNhEhqawN4Q9JNuAt2UoM6",
    LAUNCHPAD_PROGRAM_ID
);
suite!(
    pool,
    "nBB3wTa7qDsv2Pkrwg9zoNfWvcjtQ9J2MmdTPhnVW1Y",
    DEX_PROGRAM_ID
);
