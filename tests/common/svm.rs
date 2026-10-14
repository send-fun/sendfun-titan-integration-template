//! LiteSVM 0.14 behind the LiteSVM 0.6 API and the Solana 2.2 types that the
//! suite uses.
//!
//! send.fun's programs read their instruction data from register r2
//! (SIMD-0321). LiteSVM 0.7.1 and earlier do not set it.
//!
//! - A transaction serializes with bincode as Solana 2.2 and deserializes as
//!   Solana 3. The wire format is the same.
//! - Accounts, the clock, the blockhash and the compute budget convert field
//!   by field.

use std::path::Path;

use solana_account::Account;
use solana_account_4::ReadableAccount as _;
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_sysvar::clock::Clock;
use solana_transaction::Transaction;

/// A LiteSVM 0.14 instance.
pub struct LiteSVM(litesvm::LiteSVM);

/// The accounts after a simulated transaction, and its logs.
pub struct SimulatedTransactionInfo {
    pub post_accounts: Vec<(Pubkey, Account)>,
    pub logs: Vec<String>,
}

/// A LiteSVM error. A failed transaction includes its logs.
#[derive(Debug)]
pub struct Failed(pub String);

fn failed(failure: litesvm::types::FailedTransactionMetadata) -> Failed {
    Failed(format!("{:?}; logs: {:#?}", failure.err, failure.meta.logs))
}

fn transaction(transaction: &Transaction) -> solana_transaction_4::Transaction {
    let wire = bincode::serialize(transaction).expect("serialize the transaction");
    bincode::deserialize(&wire).expect("deserialize the transaction as Solana 3")
}

fn account(account: &solana_account_4::AccountSharedData) -> Account {
    Account {
        lamports: account.lamports(),
        data: account.data().to_vec(),
        owner: Pubkey::new_from_array(account.owner().to_bytes()),
        executable: account.executable(),
        rent_epoch: account.rent_epoch(),
    }
}

impl LiteSVM {
    pub fn new() -> Self {
        Self(litesvm::LiteSVM::new())
    }

    /// Takes only `compute_unit_limit` from `compute_budget`. The other fields
    /// are LiteSVM's defaults with SIMD-0268's CPI depth active.
    pub fn with_compute_budget(self, compute_budget: ComputeBudget) -> Self {
        Self(
            self.0
                .with_compute_budget(solana_compute_budget_4::compute_budget::ComputeBudget {
                    compute_unit_limit: compute_budget.compute_unit_limit,
                    ..solana_compute_budget_4::compute_budget::ComputeBudget::new_with_defaults(
                        true,
                    )
                }),
        )
    }

    pub fn with_blockhash_check(self, check: bool) -> Self {
        Self(self.0.with_blockhash_check(check))
    }

    pub fn with_sigverify(self, sigverify: bool) -> Self {
        Self(self.0.with_sigverify(sigverify))
    }

    pub fn with_transaction_history(self, capacity: usize) -> Self {
        Self(self.0.with_transaction_history(capacity))
    }

    pub fn add_program_from_file(
        &mut self,
        program_id: Pubkey,
        path: impl AsRef<Path>,
    ) -> Result<(), Failed> {
        self.0
            .add_program_from_file(program_id.to_bytes(), path)
            .map_err(|error| Failed(format!("{error:?}")))
    }

    pub fn set_account(&mut self, pubkey: Pubkey, account: Account) -> Result<(), Failed> {
        self.0
            .set_account(
                pubkey.to_bytes().into(),
                solana_account_4::Account {
                    lamports: account.lamports,
                    data: account.data,
                    owner: account.owner.to_bytes().into(),
                    executable: account.executable,
                    rent_epoch: account.rent_epoch,
                },
            )
            .map_err(|error| Failed(format!("{error:?}")))
    }

    pub fn get_account(&self, pubkey: &Pubkey) -> Option<Account> {
        self.0
            .get_account(&pubkey.to_bytes().into())
            .map(|found| account(&found.into()))
    }

    pub fn set_clock(&mut self, clock: &Clock) {
        self.0.set_sysvar(&solana_clock_3::Clock {
            slot: clock.slot,
            epoch_start_timestamp: clock.epoch_start_timestamp,
            epoch: clock.epoch,
            leader_schedule_epoch: clock.leader_schedule_epoch,
            unix_timestamp: clock.unix_timestamp,
        });
    }

    pub fn latest_blockhash(&self) -> Hash {
        Hash::new_from_array(self.0.latest_blockhash().to_bytes())
    }

    pub fn simulate_transaction(
        &self,
        tx: Transaction,
    ) -> Result<SimulatedTransactionInfo, Failed> {
        let simulated = self
            .0
            .simulate_transaction(transaction(&tx))
            .map_err(failed)?;
        Ok(SimulatedTransactionInfo {
            logs: simulated.meta.logs.clone(),
            post_accounts: simulated
                .post_accounts
                .iter()
                .map(|(address, post)| (Pubkey::new_from_array(address.to_bytes()), account(post)))
                .collect(),
        })
    }

    pub fn send_transaction(&mut self, tx: Transaction) -> Result<Vec<String>, Failed> {
        self.0
            .send_transaction(transaction(&tx))
            .map(|meta| meta.logs)
            .map_err(failed)
    }
}
