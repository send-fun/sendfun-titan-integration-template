//! Runs the swap-route suite on a bonding curve and a DEX pool of
//! hostile.xyz's platform, `6fBeMZFvaq4sNXNGNpKW1wNZgN42v8M3NxA4bCrHNjam`.
//! Skips without `SOLANA_RPC_URL` or a current `make build-program` build.

mod common;

use common::{RouteConfig, run_swap_route};
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::sendfun::{
    DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, SendfunVenue, set_partner,
};

/// send.fun's own partner key, registered on every platform. The simulations
/// route as it. Titan routes as the key that send.fun registers for Titan.
const PARTNER: Pubkey = pubkey!("1TZSDDRKxjgEFatTYAbSgM8kxZbSAjTRPZxHYd7send");

async fn both_directions(pool: Pubkey, program: Pubkey) {
    set_partner(PARTNER).unwrap();
    run_swap_route::<SendfunVenue>(RouteConfig {
        pool,
        venue_programs: vec![program],
    })
    .await;
}

#[tokio::test]
async fn curve_both_directions() {
    both_directions(
        pubkey!("DgH8nF1MiQrTiCeNF6fYfAsNhEhqawN4Q9JNuAt2UoM6"),
        LAUNCHPAD_PROGRAM_ID,
    )
    .await;
}

#[tokio::test]
async fn pool_both_directions() {
    both_directions(
        pubkey!("nBB3wTa7qDsv2Pkrwg9zoNfWvcjtQ9J2MmdTPhnVW1Y"),
        DEX_PROGRAM_ID,
    )
    .await;
}
