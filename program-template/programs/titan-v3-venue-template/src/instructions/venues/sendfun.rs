use anchor_lang::{prelude::*, solana_program::instruction::Instruction};

/// The send.fun launchpad program. Its markets are bonding curves.
pub const LAUNCHPAD_PROGRAM_ID: Pubkey = pubkey!("5R1uFyEE4oxqkm7hJDqHq6gXaLVF9dxeF3LF4yz1sfLP");
/// The send.fun DEX program. Its markets are the pools that completed curves
/// migrate into.
pub const DEX_PROGRAM_ID: Pubkey = pubkey!("84qj5FPZZdXkQy8mfowyg6RBZ3XKuTds6XS4ZYT1sfDX");

/// `buy_exact_in` in both programs. A buy spends the quote mint.
const BUY_EXACT_IN: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];
/// `sell_exact_in` in both programs. A sell spends the base mint.
const SELL_EXACT_IN: [u8; 8] = [149, 39, 222, 155, 211, 124, 152, 26];

/// Builds one `buy_exact_in` instruction if `buy`, else one `sell_exact_in`,
/// that spends `amount_in`. The program is the DEX if `dex`, else the
/// launchpad. The instruction takes `account_metas` unchanged.
///
/// `min_amount_out` is 0, so the program does not check slippage. A buy past a
/// curve's supply cap fills the rest of the supply. The unspent input stays in
/// TitanPDA's account.
///
/// Never returns an error.
pub fn swap_exact_in(
    dex: bool,
    buy: bool,
    amount_in: u64,
    account_metas: &[AccountMeta],
) -> Result<Vec<Instruction>> {
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(if buy { &BUY_EXACT_IN } else { &SELL_EXACT_IN });
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());

    Ok(vec![Instruction {
        program_id: if dex {
            DEX_PROGRAM_ID
        } else {
            LAUNCHPAD_PROGRAM_ID
        },
        accounts: account_metas.to_vec(),
        data,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_serializes_the_exact_in_instruction_of_each_program() {
        let metas = [
            AccountMeta::new(Pubkey::new_unique(), true),
            AccountMeta::new_readonly(Pubkey::new_unique(), false),
        ];
        for (dex, buy, program_id, discriminator) in [
            (false, true, LAUNCHPAD_PROGRAM_ID, BUY_EXACT_IN),
            (false, false, LAUNCHPAD_PROGRAM_ID, SELL_EXACT_IN),
            (true, true, DEX_PROGRAM_ID, BUY_EXACT_IN),
            (true, false, DEX_PROGRAM_ID, SELL_EXACT_IN),
        ] {
            let instructions = swap_exact_in(dex, buy, 1_234_567, &metas).unwrap();
            assert_eq!(instructions.len(), 1);
            let instruction = &instructions[0];
            assert_eq!(instruction.program_id, program_id);
            assert_eq!(instruction.accounts, metas.to_vec());
            let mut data = discriminator.to_vec();
            data.extend_from_slice(&1_234_567u64.to_le_bytes());
            data.extend_from_slice(&0u64.to_le_bytes());
            assert_eq!(instruction.data, data);
        }
    }
}
