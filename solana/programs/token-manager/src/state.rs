//! State structures for token manager program

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::msg;
use solana_program::program_error::ProgramError;
use solana_program::program_pack::{Pack, Sealed};

/// Represents a Token Manager Account in the Solana blockchain.
///
/// This struct is used to manage the flow of tokens in a Solana program. It
/// keeps track of the incoming tokens (`flow_in`), outgoing tokens
/// (`flow_out`), and the maximum allowed tokens that can flow (`flow_limit`).
/// ```
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct TokenManagerAccount {
    /// The total number of tokens that have flowed into the account.
    pub flow_limit: u64,
}

impl Sealed for TokenManagerAccount {}
impl Pack for TokenManagerAccount {
    const LEN: usize = 8;

    fn pack_into_slice(&self, dst: &mut [u8]) {
        let data = self.try_to_vec().unwrap();
        dst[..data.len()].copy_from_slice(&data);
    }

    fn unpack_from_slice(src: &[u8]) -> Result<Self, solana_program::program_error::ProgramError> {
        let mut mut_src: &[u8] = src;
        Self::deserialize(&mut mut_src).map_err(|err| {
            msg!("Error: failed to deserialize account: {}", err);
            ProgramError::InvalidAccountData
        })
    }
}

/// Represents Flow In and Flow Out in the account state
#[repr(C)]
#[derive(Clone, Debug, Default, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct FlowInOutAccount {
    /// The total number of tokens that have flowed into the account.
    pub flow_in: u64,
    /// The total number of tokens that have flowed out of the account.
    pub flow_out: u64,
}

impl Sealed for FlowInOutAccount {}
impl Pack for FlowInOutAccount {
    const LEN: usize = 16;

    fn pack_into_slice(&self, dst: &mut [u8]) {
        let data = self.try_to_vec().unwrap();
        dst[..data.len()].copy_from_slice(&data);
    }

    fn unpack_from_slice(src: &[u8]) -> Result<Self, solana_program::program_error::ProgramError> {
        let mut mut_src: &[u8] = src;
        Self::deserialize(&mut mut_src).map_err(|err| {
            msg!("Error: failed to deserialize account: {}", err);
            ProgramError::InvalidAccountData
        })
    }
}
