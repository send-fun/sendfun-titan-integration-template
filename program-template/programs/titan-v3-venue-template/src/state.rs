use anchor_lang::prelude::*;

pub const MAX_SWAPS: usize = 12;
pub const MAX_MINTS: usize = 12;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Copy, Eq, Debug)]
pub enum Venue {
    RaydiumAmm,
    /// A send.fun market: a DEX pool if `dex`, else a launchpad bonding curve.
    /// A buy if `buy`, else a sell. A buy spends the quote mint. A sell spends
    /// the base mint.
    Sendfun {
        dex: bool,
        buy: bool,
    },
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Copy, Eq, Debug)]
pub struct SwapSpecInputV2 {
    pub venue: Venue,
    pub from: u8,
    pub to: u8,
    pub weight_nanos: u32,
    pub n_accounts: u8,
}

#[account]
pub struct TitanPda {
    pub bump: u8,
}

impl TitanPda {
    pub const SIZE: usize = 1;
    pub const SEED: &'static [u8] = b"titan_pda";
}
