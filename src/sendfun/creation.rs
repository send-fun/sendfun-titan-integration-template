//! Parses market creations from instruction data and account lists.

use solana_pubkey::Pubkey;

use super::market::MarketKind;
use super::{DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID};
use crate::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};

/// `create_token` in both programs.
const CREATE_TOKEN: [u8; 8] = [84, 52, 204, 228, 24, 140, 234, 75];
/// The DEX's `create_pool`. The launchpad's `migrate` calls it by CPI. The
/// DEX authority calls it directly.
const CREATE_POOL: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];

/// Where a creation instruction names the new market and its mints.
struct Layout {
    kind: MarketKind,
    base_mint: usize,
    quote_mint: usize,
    market: usize,
}

const LAUNCHPAD_CREATE_TOKEN: Layout = Layout {
    kind: MarketKind::Launchpad,
    base_mint: 4,
    quote_mint: 5,
    market: 6,
};

const DEX_CREATE_TOKEN: Layout = Layout {
    kind: MarketKind::Dex,
    base_mint: 3,
    quote_mint: 4,
    market: 5,
};

const DEX_CREATE_POOL: Layout = Layout {
    kind: MarketKind::Dex,
    base_mint: 7,
    quote_mint: 8,
    market: 9,
};

fn layout(instruction: &ParsedInstruction) -> Option<&'static Layout> {
    let discriminator = instruction.data.first_chunk::<8>()?;
    match (instruction.program_id, *discriminator) {
        (LAUNCHPAD_PROGRAM_ID, CREATE_TOKEN) => Some(&LAUNCHPAD_CREATE_TOKEN),
        (DEX_PROGRAM_ID, CREATE_TOKEN) => Some(&DEX_CREATE_TOKEN),
        (DEX_PROGRAM_ID, CREATE_POOL) => Some(&DEX_CREATE_POOL),
        _ => None,
    }
}

fn creation(instruction: &ParsedInstruction) -> Option<PoolCreation> {
    let layout = layout(instruction)?;
    let account = |index: usize| instruction.accounts.get(index).copied();
    let base_mint: Pubkey = account(layout.base_mint)?;
    let quote_mint = account(layout.quote_mint)?;
    Some(PoolCreation {
        protocol: layout.kind.protocol(),
        pool: account(layout.market)?,
        mints: vec![base_mint, quote_mint],
    })
}

/// Returns each curve and pool that `instructions` create, in order.
///
/// Include inner instructions. A migration creates its pool by CPI.
///
/// - launchpad `create_token`: a bonding curve.
/// - DEX `create_token`: a pool with a new token.
/// - DEX `create_pool`: a pool for existing mints.
///
/// The mints are `[base, quote]`, the order of
/// [`SendfunVenue::get_token_info`](crate::trading_venue::TradingVenue::get_token_info).
/// An instruction with too few accounts or data under 8 bytes is skipped.
pub fn parse_pool_creations(instructions: &[ParsedInstruction]) -> Vec<PoolCreation> {
    instructions.iter().filter_map(creation).collect()
}
