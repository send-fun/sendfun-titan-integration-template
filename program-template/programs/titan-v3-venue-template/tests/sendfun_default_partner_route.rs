//! Runs the swap-route suite on every send.fun test market, routed as the
//! default partner: the System Program key. It signs nothing and pays each
//! platform's unpartnered schedule. Skips without `SOLANA_RPC_URL` or a
//! current `make build-program` build.

mod common;

use common::{RouteConfig, run_swap_route};
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::sendfun::{
    DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, SendfunVenue, set_partner,
};

const DEFAULT_PARTNER: Pubkey = pubkey!("11111111111111111111111111111111");

async fn both_directions(pool: Pubkey, program: Pubkey) {
    set_partner(DEFAULT_PARTNER).unwrap();
    run_swap_route::<SendfunVenue>(RouteConfig {
        pool,
        venue_programs: vec![program],
    })
    .await;
}

#[tokio::test]
async fn curve_wsol_both_directions() {
    both_directions(
        pubkey!("Eq52yham9BNMbz2pCcqpSFUNAfJZKJFPNEhqHaEdyy7L"),
        LAUNCHPAD_PROGRAM_ID,
    )
    .await;
}

#[tokio::test]
async fn curve_fee_both_directions() {
    both_directions(
        pubkey!("ECxRrNU6Ggd73c3AD7UPd6ijfJYdULWFT2jDJZu5gzcd"),
        LAUNCHPAD_PROGRAM_ID,
    )
    .await;
}

#[tokio::test]
async fn dex_pool_both_directions() {
    both_directions(
        pubkey!("ChJu4A7Gow1DDsWdgHvcpJtxDQQZvA5tavWKn8zZ9kY5"),
        DEX_PROGRAM_ID,
    )
    .await;
}

// On hostile.xyz's platform.
#[tokio::test]
async fn hostile_curve_both_directions() {
    both_directions(
        pubkey!("DgH8nF1MiQrTiCeNF6fYfAsNhEhqawN4Q9JNuAt2UoM6"),
        LAUNCHPAD_PROGRAM_ID,
    )
    .await;
}

#[tokio::test]
async fn hostile_pool_both_directions() {
    both_directions(
        pubkey!("nBB3wTa7qDsv2Pkrwg9zoNfWvcjtQ9J2MmdTPhnVW1Y"),
        DEX_PROGRAM_ID,
    )
    .await;
}
