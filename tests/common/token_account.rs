//! Token accounts laid out as their mint's token program requires.
//!
//! An SPL Token account is 165 bytes. A Token-2022 account also carries each
//! account extension that its mint requires
//! (`ExtensionType::get_required_init_account_extensions`). Token-2022 fails a
//! transfer of a fee-bearing mint into an account without
//! `TransferFeeAmount`. It fails a transfer of a hooked mint unless both
//! accounts carry `TransferHookAccount`.

use solana_account::Account;
use solana_program::native_token::LAMPORTS_PER_SOL;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use spl_token_2022::extension::{
    BaseStateWithExtensions, BaseStateWithExtensionsMut, ExtensionType, StateWithExtensions,
    StateWithExtensionsMut,
};
use spl_token_2022::state::{Account as TokenAccount, AccountState, Mint};

/// An initialized token account of `mint`, held by `owner` with `amount`.
/// `mint_account` is the mint's account. Its owner sets the token program.
/// Panics on a Token-2022 mint that does not unpack.
pub fn token_account(
    mint: &Pubkey,
    mint_account: &Account,
    owner: &Pubkey,
    amount: u64,
) -> Account {
    let base = TokenAccount {
        mint: *mint,
        owner: *owner,
        amount,
        state: AccountState::Initialized,
        ..Default::default()
    };
    let token_program = mint_account.owner;
    if token_program != spl_token_2022::ID {
        let mut account = Account::new(LAMPORTS_PER_SOL, TokenAccount::LEN, &token_program);
        base.pack_into_slice(&mut account.data);
        return account;
    }

    let mint_extensions = StateWithExtensions::<Mint>::unpack(&mint_account.data)
        .and_then(|mint| mint.get_extension_types())
        .expect("Token-2022 mint extensions");
    let required = ExtensionType::get_required_init_account_extensions(&mint_extensions);
    let len = ExtensionType::try_calculate_account_len::<TokenAccount>(&required)
        .expect("token account length");
    let mut account = Account::new(LAMPORTS_PER_SOL, len, &token_program);
    let mut state = StateWithExtensionsMut::<TokenAccount>::unpack_uninitialized(&mut account.data)
        .expect("uninitialized token account");
    for extension in required {
        state
            .init_account_extension_from_type(extension)
            .expect("token account extension");
    }
    state.base = base;
    state.pack_base();
    state.init_account_type().expect("token account type");
    account
}
