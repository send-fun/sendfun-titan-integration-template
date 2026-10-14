//! send.fun's creation parser on fixtures of each creation instruction, in
//! on-chain account order. Needs no RPC.
//!
//! - launchpad `create_token`: a new bonding curve.
//! - DEX `create_pool`: a pool for existing mints. The launchpad's `migrate`
//!   calls it by CPI. The DEX authority calls it directly.
//! - DEX `create_token`: a pool with a new token.

use solana_pubkey::{Pubkey, pubkey};

use sendfun_sdk::launchpad::create::{BuildCreateTokenParams, build_create_token_instruction};
use titan_integration_template::trading_venue::protocol::PoolProtocol;
use titan_integration_template::trading_venue::venue_creation::{ParsedInstruction, PoolCreation};
use titan_integration_template::your_venue::{
    DEX_PROGRAM_ID, LAUNCHPAD_PROGRAM_ID, parse_pool_creations,
};

/// A mainnet bonding curve quoted in WSOL, and its mints.
const CURVE: Pubkey = pubkey!("Eq52yham9BNMbz2pCcqpSFUNAfJZKJFPNEhqHaEdyy7L");
const CURVE_BASE_MINT: Pubkey = pubkey!("Sg18DfCS5Ne8JujHcCsidTctSacmAhRXKM6BCG4send");

/// A mainnet DEX pool that a curve migrated into, and its mints.
const POOL: Pubkey = pubkey!("ChJu4A7Gow1DDsWdgHvcpJtxDQQZvA5tavWKn8zZ9kY5");
const POOL_BASE_MINT: Pubkey = pubkey!("eRL5z1LmGvJxXj8MxFZSNXsukVgF23egb6ZxXnRsend");

const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
const TOKEN_PROGRAM: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// The platform both markets were created on.
const PLATFORM_CONFIG: Pubkey = pubkey!("2PJedAsa7pCnScks2XM3U4o2nPaTvvBmi2553VcCnGWB");

const CREATE_TOKEN: [u8; 8] = [84, 52, 204, 228, 24, 140, 234, 75];
const CREATE_POOL: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];
const BUY_EXACT_IN: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];

const fn address(pubkey: &Pubkey) -> solana_address::Address {
    solana_address::Address::new_from_array(pubkey.to_bytes())
}

fn string(data: &mut Vec<u8>, value: &str) {
    data.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
    data.extend_from_slice(value.as_bytes());
}

/// The launchpad's `create_token` for the WSOL curve, as the SDK builds it.
fn launchpad_create_token() -> ParsedInstruction {
    let user = address(&Pubkey::new_unique());
    let instruction = build_create_token_instruction(&BuildCreateTokenParams {
        user: &user,
        payer: None,
        coin_creator: &user,
        base_mint: &address(&CURVE_BASE_MINT),
        quote_mint: &address(&WSOL_MINT),
        partner: &address(&Pubkey::default()),
        platform_config: &address(&PLATFORM_CONFIG),
        quote_token_program: &address(&TOKEN_PROGRAM),
        name: "Send",
        symbol: "SEND",
        uri: "https://example.com/send.json",
        creator_fee_mode: 0,
        creator_fee_bps: 0,
        dex_creator_fee_bps: 10,
    });
    ParsedInstruction {
        program_id: Pubkey::new_from_array(instruction.program_id.to_bytes()),
        accounts: instruction
            .accounts
            .iter()
            .map(|meta| Pubkey::new_from_array(meta.pubkey.to_bytes()))
            .collect(),
        data: instruction.data,
    }
}

/// The DEX's `create_pool`: `caller, global_config, authority,
/// migration_authority, source_authority, source_base_account,
/// source_quote_account, base_mint, quote_mint, pool, lp_mint, base_vault,
/// quote_vault, pool_lp_account, base_token_program, quote_token_program,
/// associated_token_program, system_program, nexus_program,
/// creator_fee_config, event_authority, program`.
fn dex_create_pool(base_mint: Pubkey, quote_mint: Pubkey, pool: Pubkey) -> ParsedInstruction {
    let mut accounts: Vec<Pubkey> = (0..22).map(|_| Pubkey::new_unique()).collect();
    accounts[7] = base_mint;
    accounts[8] = quote_mint;
    accounts[9] = pool;
    accounts[21] = DEX_PROGRAM_ID;

    // `CreatePoolArgs`: initial_base, initial_quote, coin_creator, created_at,
    // platform_config, creator_fee_mode, creator_fee_bps.
    let mut data = CREATE_POOL.to_vec();
    data.extend_from_slice(&206_900_000_000_000_u64.to_le_bytes());
    data.extend_from_slice(&85_000_000_000_u64.to_le_bytes());
    data.extend_from_slice(Pubkey::new_unique().as_ref());
    data.extend_from_slice(&1_758_000_000_i64.to_le_bytes());
    data.extend_from_slice(PLATFORM_CONFIG.as_ref());
    data.push(0);
    data.extend_from_slice(&10_u16.to_le_bytes());
    ParsedInstruction {
        program_id: DEX_PROGRAM_ID,
        accounts,
        data,
    }
}

/// The DEX's `create_token`: `user, payer, global_config, base_mint,
/// quote_mint, pool, ...`, 29 accounts.
fn dex_create_token(base_mint: Pubkey, quote_mint: Pubkey, pool: Pubkey) -> ParsedInstruction {
    let mut accounts: Vec<Pubkey> = (0..29).map(|_| Pubkey::new_unique()).collect();
    accounts[3] = base_mint;
    accounts[4] = quote_mint;
    accounts[5] = pool;
    accounts[28] = DEX_PROGRAM_ID;

    // `platform_config`, then `CreateTokenArgs`: base_supply, initial_quote,
    // name, symbol, uri, creator_fee_mode, creator_fee_bps, coin_creator.
    let mut data = CREATE_TOKEN.to_vec();
    data.extend_from_slice(PLATFORM_CONFIG.as_ref());
    data.extend_from_slice(&1_000_000_000_000_000_u64.to_le_bytes());
    data.extend_from_slice(&1_000_000_000_u64.to_le_bytes());
    string(&mut data, "Send");
    string(&mut data, "SEND");
    string(&mut data, "https://example.com/send.json");
    data.push(0);
    data.extend_from_slice(&10_u16.to_le_bytes());
    data.extend_from_slice(Pubkey::new_unique().as_ref());
    ParsedInstruction {
        program_id: DEX_PROGRAM_ID,
        accounts,
        data,
    }
}

/// A trade on the pool: a send.fun instruction that creates nothing.
fn dex_buy() -> ParsedInstruction {
    let mut data = BUY_EXACT_IN.to_vec();
    data.extend_from_slice(&1_000_000_000_u64.to_le_bytes());
    data.extend_from_slice(&0_u64.to_le_bytes());
    let mut accounts: Vec<Pubkey> = (0..17).map(|_| Pubkey::new_unique()).collect();
    accounts[2] = POOL;
    ParsedInstruction {
        program_id: DEX_PROGRAM_ID,
        accounts,
        data,
    }
}

/// The launchpad's `migrate`. It creates nothing. Its inner `create_pool`
/// creates the pool.
fn launchpad_migrate() -> ParsedInstruction {
    let mut accounts: Vec<Pubkey> = (0..23).map(|_| Pubkey::new_unique()).collect();
    accounts[1] = CURVE;
    accounts[8] = POOL;
    ParsedInstruction {
        program_id: LAUNCHPAD_PROGRAM_ID,
        accounts,
        data: vec![155, 234, 231, 146, 236, 158, 162, 30],
    }
}

#[test]
fn parses_a_launchpad_curve_creation() {
    assert_eq!(
        parse_pool_creations(&[launchpad_create_token()]),
        vec![PoolCreation {
            protocol: PoolProtocol::SendfunLaunchpad,
            pool: CURVE,
            mints: vec![CURVE_BASE_MINT, WSOL_MINT],
        }],
    );
}

#[test]
fn parses_a_migration_pool_from_its_inner_instruction() {
    let creations = parse_pool_creations(&[
        launchpad_migrate(),
        dex_create_pool(POOL_BASE_MINT, WSOL_MINT, POOL),
    ]);
    assert_eq!(
        creations,
        vec![PoolCreation {
            protocol: PoolProtocol::SendfunAmm,
            pool: POOL,
            mints: vec![POOL_BASE_MINT, WSOL_MINT],
        }],
    );
}

#[test]
fn parses_a_dex_token_creation() {
    let base_mint = Pubkey::new_unique();
    let pool = Pubkey::new_unique();
    assert_eq!(
        parse_pool_creations(&[dex_buy(), dex_create_token(base_mint, WSOL_MINT, pool)]),
        vec![PoolCreation {
            protocol: PoolProtocol::SendfunAmm,
            pool,
            mints: vec![base_mint, WSOL_MINT],
        }],
    );
}

#[test]
fn parses_every_creation_in_order() {
    let creations = parse_pool_creations(&[
        launchpad_create_token(),
        dex_buy(),
        dex_create_pool(POOL_BASE_MINT, WSOL_MINT, POOL),
    ]);
    assert_eq!(
        creations
            .iter()
            .map(|creation| (creation.protocol, creation.pool))
            .collect::<Vec<_>>(),
        vec![
            (PoolProtocol::SendfunLaunchpad, CURVE),
            (PoolProtocol::SendfunAmm, POOL),
        ],
    );
}

#[test]
fn ignores_transactions_without_a_creation() {
    let creations = parse_pool_creations(&[dex_buy(), launchpad_migrate()]);
    assert!(
        creations.is_empty(),
        "a transaction without a market creation creates no markets, got {creations:?}"
    );
}

#[test]
fn ignores_a_creation_discriminator_under_another_program() {
    let mut instruction = dex_create_pool(POOL_BASE_MINT, WSOL_MINT, POOL);
    instruction.program_id = Pubkey::new_unique();
    let mut launchpad_pool = dex_create_pool(POOL_BASE_MINT, WSOL_MINT, POOL);
    // The launchpad has no `create_pool`.
    launchpad_pool.program_id = LAUNCHPAD_PROGRAM_ID;
    assert_eq!(parse_pool_creations(&[instruction, launchpad_pool]), vec![]);
}

#[test]
fn skips_a_creation_with_too_few_accounts() {
    let mut instruction = dex_create_pool(POOL_BASE_MINT, WSOL_MINT, POOL);
    instruction.accounts.truncate(9);
    assert_eq!(parse_pool_creations(&[instruction]), vec![]);
    let short_data = ParsedInstruction {
        program_id: DEX_PROGRAM_ID,
        accounts: vec![Pubkey::new_unique(); 22],
        data: CREATE_POOL[..7].to_vec(),
    };
    assert_eq!(parse_pool_creations(&[short_data]), vec![]);
}
