//! Key conversion between the template's Solana 2.2 types and the SDK's
//! Solana 3 types.

use solana_address::Address;
use solana_pubkey::Pubkey;

pub const fn to_address(pubkey: &Pubkey) -> Address {
    Address::new_from_array(pubkey.to_bytes())
}

pub const fn to_pubkey(address: &Address) -> Pubkey {
    Pubkey::new_from_array(address.to_bytes())
}
