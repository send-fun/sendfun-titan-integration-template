//! Runs the swap-route suite on a send.fun launchpad bonding curve quoted in
//! WSOL. Skips without `SOLANA_RPC_URL` or a current `make build-program`
//! build.

mod common;

use common::{RouteConfig, run_swap_route};
use solana_pubkey::{Pubkey, pubkey};
use titan_integration_template::sendfun::{LAUNCHPAD_PROGRAM_ID, SendfunVenue, set_partner};

/// send.fun's own partner key, registered on every platform. The simulations
/// route as it. Titan routes as the key that send.fun registers for Titan.
const PARTNER: Pubkey = pubkey!("1TZSDDRKxjgEFatTYAbSgM8kxZbSAjTRPZxHYd7send");

#[tokio::test]
async fn swap_route_both_directions() {
    set_partner(PARTNER).unwrap();
    run_swap_route::<SendfunVenue>(RouteConfig {
        pool: pubkey!("Eq52yham9BNMbz2pCcqpSFUNAfJZKJFPNEhqHaEdyy7L"),
        venue_programs: vec![LAUNCHPAD_PROGRAM_ID],
    })
    .await;
}
