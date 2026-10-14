//! Shared, venue-generic swap-route suite for the on-chain template.
//!
//! Both `tests/example_route.rs` (the Raydium reference) and
//! `tests/your_venue_route.rs` (your integration) call [`run_swap_route`], so
//! the example and your venue are exercised by the same end-to-end test:
//! quote off-chain, execute `swap_route_v3` in LiteSVM, and
//! assert the simulated output matches the quote.
//!
//! It runtime-SKIPs (prints a reason and returns) when prerequisites are
//! missing, so `cargo test` stays clean on a fresh clone. It needs:
//!   - `SOLANA_RPC_URL` (mainnet);
//!   - the built route program (`make build-program` from the repo root);
//!   - the venue program binaries (auto-dumped via the `solana` CLI).

#![allow(dead_code)] // each test binary uses a subset of these helpers.

#[path = "../../../../../tests/common/token_account.rs"]
mod token_account;

#[path = "../../../../../tests/common/svm.rs"]
mod svm;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use solana_account::{Account, ReadableAccount};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_instruction::{AccountMeta, Instruction};
use solana_program::native_token::LAMPORTS_PER_SOL;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sysvar::clock::{self, Clock};
use solana_transaction::Transaction;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token::state::Account as TokenAccount;
use spl_token_2022::extension::transfer_fee::TransferFeeConfig;
use spl_token_2022::extension::{BaseStateWithExtensions, StateWithExtensions};
use spl_token_2022::state::Mint;
use svm::LiteSVM;
use titan_integration_template::account_caching::AccountsCache;
use titan_integration_template::account_caching::rpc_cache::RpcClientCache;
use titan_integration_template::swap_route::{
    ROUTE_WEIGHT_ALL, build_swap_leg, encode_swap_route_v3_data,
};
use titan_integration_template::trading_venue::error::TradingVenueError;
use titan_integration_template::trading_venue::token_info::TokenInfo;
use titan_integration_template::trading_venue::{
    FromAccount, QuoteRequest, SwapType, TradingVenue,
};
use titan_v3_venue_template::state::TitanPda;

const SAMPLE_COUNT: usize = 10;

/// A venue usable by the route suite: buildable from an account, quotable, and
/// usable across `.await`.
pub trait RouteVenue: TradingVenue + FromAccount + Send + Sync {}
impl<T: TradingVenue + FromAccount + Send + Sync> RouteVenue for T {}

/// Per-venue configuration for the route suite.
pub struct RouteConfig {
    /// Pool/market account the venue is constructed from.
    pub pool: Pubkey,
    /// The venue's program(s) the swap CPI invokes. Each is dumped to
    /// `program-dumps/<id>.so` and loaded into LiteSVM.
    pub venue_programs: Vec<Pubkey>,
}

fn init_test_logger() {
    let _ = env_logger::builder().is_test(true).try_init();
}

fn current_test() -> String {
    std::thread::current()
        .name()
        .unwrap_or("a route test")
        .to_string()
}

fn workspace_path(path: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn newest_rust_source_mtime(dir: &Path) -> Result<SystemTime, String> {
    let mut newest = SystemTime::UNIX_EPOCH;
    for entry in fs::read_dir(dir)
        .map_err(|e| format!("failed to read source dir {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("failed to read source entry: {e}"))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|e| format!("failed to stat {}: {e}", path.display()))?;

        if metadata.is_dir() {
            newest = newest.max(newest_rust_source_mtime(&path)?);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            newest = newest.max(
                metadata
                    .modified()
                    .map_err(|e| format!("failed to read mtime for {}: {e}", path.display()))?,
            );
        }
    }
    Ok(newest)
}

fn ensure_route_program_is_fresh(route_so: &Path) -> Result<(), String> {
    let route_mtime = fs::metadata(route_so)
        .and_then(|metadata| metadata.modified())
        .map_err(|e| {
            format!(
                "failed to stat built route program {}: {e}",
                route_so.display()
            )
        })?;
    let src_dir = workspace_path("programs/titan-v3-venue-template/src");
    let source_mtime = newest_rust_source_mtime(&src_dir)?;

    if route_mtime < source_mtime {
        return Err(format!(
            "stale built route program {} — run `make build-program` from the repo root",
            route_so.display()
        ));
    }

    Ok(())
}

/// Dump `program` to `path` via the Solana CLI if it isn't already there.
fn ensure_program_dump(program: Pubkey, path: &Path, rpc_url: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create dump dir: {e}"))?;
    }
    let output = Command::new("solana")
        .args(["--url", rpc_url, "program", "dump", &program.to_string()])
        .arg(path)
        .output()
        .map_err(|e| {
            format!("failed to run `solana program dump` (is the Solana CLI installed?): {e}")
        })?;
    if !output.status.success() {
        return Err(format!(
            "`solana program dump {program}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn setup_litesvm() -> (LiteSVM, Keypair) {
    let mut litesvm = LiteSVM::new()
        .with_compute_budget(ComputeBudget {
            compute_unit_limit: 1_400_000,
            ..Default::default()
        })
        .with_blockhash_check(false)
        .with_sigverify(false)
        .with_transaction_history(0);

    let payer = Keypair::new();
    let account = Account {
        lamports: 10_000 * LAMPORTS_PER_SOL,
        data: vec![],
        owner: solana_sdk::system_program::id(),
        executable: false,
        rent_epoch: 0,
    };
    litesvm.set_account(payer.pubkey(), account).unwrap();
    (litesvm, payer)
}

fn token_for_mint(venue: &dyn TradingVenue, mint: Pubkey) -> &TokenInfo {
    venue
        .get_token_info()
        .iter()
        .find(|token| token.pubkey == mint)
        .expect("route mint not in venue token info")
}

/// Returns a token account of `token` with the account extensions that
/// `mint_account` requires.
fn create_token_account(
    token: &TokenInfo,
    mint_account: &Account,
    owner: Pubkey,
    amount: u64,
) -> Account {
    token_account::token_account(&token.pubkey, mint_account, &owner, amount)
}

fn initialize_titan_pda(litesvm: &mut LiteSVM, payer: &Keypair, titan_pda: Pubkey) {
    let data =
        anchor_lang::solana_program::hash::hash(b"global:initialize").to_bytes()[..8].to_vec();
    let ix = Instruction {
        program_id: titan_v3_venue_template::ID,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(titan_pda, false),
            AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        ],
        data,
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        litesvm.latest_blockhash(),
    );
    litesvm.send_transaction(tx).unwrap();
}

/// Build the `swap_route_v3` instruction for a single-leg route (mint 0 -> 1).
fn build_route_instruction(
    payer: Pubkey,
    titan_pda: Pubkey,
    venue: &dyn TradingVenue,
    request: &QuoteRequest,
) -> Instruction {
    let input_token = token_for_mint(venue, request.input_mint);
    let output_token = token_for_mint(venue, request.output_mint);
    let input_token_program = input_token.get_token_program();
    let output_token_program = output_token.get_token_program();

    let input_ata = get_associated_token_address_with_program_id(
        &payer,
        &request.input_mint,
        &input_token_program,
    );
    let output_ata = get_associated_token_address_with_program_id(
        &payer,
        &request.output_mint,
        &output_token_program,
    );
    let titan_pda_input_ata = get_associated_token_address_with_program_id(
        &titan_pda,
        &request.input_mint,
        &input_token_program,
    );
    let titan_pda_output_ata = get_associated_token_address_with_program_id(
        &titan_pda,
        &request.output_mint,
        &output_token_program,
    );

    let mut accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(titan_pda, false),
        AccountMeta::new(input_ata, false),
        AccountMeta::new(output_ata, false),
        AccountMeta::new_readonly(spl_token::ID, false),
        AccountMeta::new_readonly(spl_token_2022::ID, false),
        AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
        AccountMeta::new_readonly(spl_associated_token_account::ID, false),
        // Optional route accounts. Use this program id as the Anchor Option placeholder.
        AccountMeta::new_readonly(titan_v3_venue_template::ID, false),
        AccountMeta::new_readonly(titan_v3_venue_template::ID, false),
        AccountMeta::new_readonly(titan_v3_venue_template::ID, false),
        AccountMeta::new(titan_pda_input_ata, false),
        AccountMeta::new(titan_pda_output_ata, false),
        AccountMeta::new_readonly(request.input_mint, false),
        AccountMeta::new_readonly(request.output_mint, false),
    ];

    // Build the venue leg from the venue swap instruction: it clears the
    // TitanPDA signer flag, appends the venue program id, and computes n_accounts.
    let (spec, leg_accounts) =
        build_swap_leg(venue, request, titan_pda, 0, 1, ROUTE_WEIGHT_ALL).unwrap();
    accounts.extend(leg_accounts);

    let data = encode_swap_route_v3_data(request.amount, 2, &[spec]);
    Instruction {
        program_id: titan_v3_venue_template::ID,
        accounts,
        data,
    }
}

#[allow(clippy::too_many_arguments)]
async fn load_route_accounts(
    litesvm: &mut LiteSVM,
    cache: &RpcClientCache,
    venue: &dyn TradingVenue,
    payer: Pubkey,
    titan_pda: Pubkey,
    request: &QuoteRequest,
    swap_ix: &Instruction,
) {
    let input_token = token_for_mint(venue, request.input_mint);
    let output_token = token_for_mint(venue, request.output_mint);
    let input_token_program = input_token.get_token_program();
    let output_token_program = output_token.get_token_program();

    let input_ata = get_associated_token_address_with_program_id(
        &payer,
        &request.input_mint,
        &input_token_program,
    );
    let output_ata = get_associated_token_address_with_program_id(
        &payer,
        &request.output_mint,
        &output_token_program,
    );
    let titan_pda_input_ata = get_associated_token_address_with_program_id(
        &titan_pda,
        &request.input_mint,
        &input_token_program,
    );
    let titan_pda_output_ata = get_associated_token_address_with_program_id(
        &titan_pda,
        &request.output_mint,
        &output_token_program,
    );

    let input_mint = cache
        .get_account(&request.input_mint)
        .await
        .unwrap()
        .expect("input mint account");
    let output_mint = cache
        .get_account(&request.output_mint)
        .await
        .unwrap()
        .expect("output mint account");
    litesvm
        .set_account(
            input_ata,
            create_token_account(input_token, &input_mint, payer, u64::MAX),
        )
        .unwrap();
    litesvm
        .set_account(
            output_ata,
            create_token_account(output_token, &output_mint, payer, 0),
        )
        .unwrap();
    litesvm
        .set_account(
            titan_pda_input_ata,
            create_token_account(input_token, &input_mint, titan_pda, 0),
        )
        .unwrap();
    litesvm
        .set_account(
            titan_pda_output_ata,
            create_token_account(output_token, &output_mint, titan_pda, 0),
        )
        .unwrap();

    let latest_clock = cache.get_account(&clock::ID).await.unwrap();
    let latest_clock: Clock = latest_clock
        .as_ref()
        .ok_or(TradingVenueError::NoAccountFound(clock::ID.into()))
        .unwrap()
        .deserialize_data()
        .unwrap();
    litesvm.set_clock(&latest_clock);

    let mut accounts_to_load = vec![request.input_mint, request.output_mint];
    accounts_to_load.extend(swap_ix.accounts.iter().map(|account| account.pubkey));
    accounts_to_load.extend(venue.get_required_pubkeys_for_update().unwrap());
    accounts_to_load.sort();
    accounts_to_load.dedup();

    let accounts = cache.get_accounts(&accounts_to_load).await.unwrap();
    for (pubkey, account) in accounts_to_load.into_iter().zip(accounts) {
        if [
            input_ata,
            output_ata,
            titan_pda_input_ata,
            titan_pda_output_ata,
            payer,
            titan_pda,
        ]
        .contains(&pubkey)
        {
            continue;
        }
        if let Some(account) = account {
            if !account.executable {
                litesvm.set_account(pubkey, account).unwrap();
            }
        }
    }
}

fn simulated_output_amount(
    litesvm: &mut LiteSVM,
    payer: &Keypair,
    output_ata: Pubkey,
    ix: Instruction,
) -> u64 {
    // A leg can name a signer besides the user, such as a partner key.
    // Signature verification is off, so its signature stays empty.
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    tx.partial_sign(&[payer], litesvm.latest_blockhash());
    let simulation_result = litesvm.simulate_transaction(tx).unwrap();
    let output_account = simulation_result
        .post_accounts
        .into_iter()
        .find(|(pubkey, _)| pubkey == &output_ata)
        .map(|(_, account)| account)
        .unwrap();
    TokenAccount::unpack_from_slice(output_account.data())
        .unwrap()
        .amount
}

/// Returns the transfer fee of `mint_account` on `amount` at `epoch`. Returns 0
/// for a mint without a transfer fee.
fn transfer_fee(mint_account: &Account, epoch: u64, amount: u64) -> u64 {
    if mint_account.owner != spl_token_2022::ID {
        return 0;
    }
    let mint = StateWithExtensions::<Mint>::unpack(&mint_account.data).expect("Token-2022 mint");
    mint.get_extension::<TransferFeeConfig>()
        .map_or(0, |config| {
            config
                .calculate_epoch_fee(epoch, amount)
                .expect("transfer fee")
        })
}

fn sample_amounts(lower: u64, upper: u64) -> Vec<u64> {
    let low = lower.max(1);
    let high = upper.max(low);
    (0..SAMPLE_COUNT)
        .map(|i| {
            let numerator = (high - low) as u128 * i as u128;
            let offset = numerator / (SAMPLE_COUNT - 1) as u128;
            low.saturating_add(offset as u64)
        })
        .collect()
}

/// Execute `swap_route_v3` against the venue across every declared direction and
/// a range of sizes, asserting the simulated output matches the off-chain quote.
pub async fn run_swap_route<V: RouteVenue>(config: RouteConfig) {
    init_test_logger();

    // (a) RPC endpoint.
    let Ok(rpc_url) = env::var("SOLANA_RPC_URL") else {
        eprintln!(
            "SKIP {}: set SOLANA_RPC_URL to run this swap-route test",
            current_test()
        );
        return;
    };

    // (b) the built route program.
    let route_so = workspace_path("target/deploy/titan_v3_venue_template.so");
    if !route_so.exists() {
        eprintln!(
            "SKIP {}: missing {} — run `make build-program` from the repo root",
            current_test(),
            route_so.display()
        );
        return;
    }
    if let Err(reason) = ensure_route_program_is_fresh(&route_so) {
        eprintln!("SKIP {}: {reason}", current_test());
        return;
    }

    // (c) the venue program binaries (dumped from the network).
    let mut venue_dumps = Vec::new();
    for program in &config.venue_programs {
        let path = workspace_path(format!("program-dumps/{program}.so"));
        if let Err(reason) = ensure_program_dump(*program, &path, &rpc_url) {
            eprintln!("SKIP {}: {reason}", current_test());
            return;
        }
        venue_dumps.push((*program, path));
    }

    // Build the venue from live state.
    let rpc = RpcClient::new(rpc_url);
    let venue_account = rpc
        .get_account(&config.pool)
        .await
        .expect("failed to fetch pool account");
    let cache = RpcClientCache::new(rpc);
    let mut venue = V::from_account(&config.pool, &venue_account).expect("from_account failed");
    venue
        .update_state(&cache)
        .await
        .expect("update_state failed");

    // LiteSVM with the route program + the venue's programs loaded.
    let (mut litesvm, payer) = setup_litesvm();
    litesvm
        .add_program_from_file(titan_v3_venue_template::ID, &route_so)
        .unwrap();
    for (program, path) in &venue_dumps {
        litesvm.add_program_from_file(*program, path).unwrap();
    }

    let (titan_pda, _) =
        Pubkey::find_program_address(&[TitanPda::SEED], &titan_v3_venue_template::ID);
    initialize_titan_pda(&mut litesvm, &payer, titan_pda);

    // `load_route_accounts` sets LiteSVM to this clock. Transfer fees use its
    // epoch.
    let epoch = cache
        .get_account(&clock::ID)
        .await
        .unwrap()
        .expect("clock sysvar account")
        .deserialize_data::<Clock>()
        .unwrap()
        .epoch;

    for (input_index, output_index) in venue.directions_num() {
        let (lower, upper) = venue.bounds(input_index, output_index).unwrap();
        let input_mint = venue.get_token(input_index as usize).unwrap().pubkey;
        let output_mint = venue.get_token(output_index as usize).unwrap().pubkey;
        let input_mint_account = cache
            .get_account(&input_mint)
            .await
            .unwrap()
            .expect("input mint account");
        let output_mint_account = cache
            .get_account(&output_mint)
            .await
            .unwrap()
            .expect("output mint account");

        for amount in sample_amounts(lower, upper) {
            let request = QuoteRequest {
                input_mint,
                output_mint,
                amount,
                swap_type: SwapType::ExactIn,
            };
            // The router moves the input to TitanPDA and the output to the
            // user. Each transfer pays its mint's transfer fee. The leg spends
            // the input that reaches TitanPDA.
            let leg = venue
                .quote(QuoteRequest {
                    amount: amount - transfer_fee(&input_mint_account, epoch, amount),
                    ..request.clone()
                })
                .unwrap();
            let expected_output = leg.expected_output
                - transfer_fee(&output_mint_account, epoch, leg.expected_output);
            let swap_ix = venue
                .generate_swap_instruction(request.clone(), titan_pda)
                .unwrap();

            load_route_accounts(
                &mut litesvm,
                &cache,
                &venue,
                payer.pubkey(),
                titan_pda,
                &request,
                &swap_ix,
            )
            .await;

            let route_ix = build_route_instruction(payer.pubkey(), titan_pda, &venue, &request);
            let output_token = token_for_mint(&venue, request.output_mint);
            let output_ata = get_associated_token_address_with_program_id(
                &payer.pubkey(),
                &request.output_mint,
                &output_token.get_token_program(),
            );
            let simulated = simulated_output_amount(&mut litesvm, &payer, output_ata, route_ix);

            println!(
                "[{input_mint} -> {output_mint}] amount={amount} quote={} expected={expected_output} sim={simulated}",
                leg.expected_output
            );
            assert_eq!(
                simulated, expected_output,
                "simulated output {simulated} != expected {expected_output} (leg quote {}) at amount {amount}",
                leg.expected_output
            );
        }
    }
}
