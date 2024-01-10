//! Program state processor.

use auth_weighted::error::AuthWeightedError;
use auth_weighted::types::account::state::AuthWeightedStateAccount;
use auth_weighted::types::account::transfer_operatorship::TransferOperatorshipAccount;
use borsh::from_slice;
use solana_program::account_info::{next_account_info, AccountInfo};
use solana_program::entrypoint::ProgramResult;
use solana_program::keccak::hash;
use solana_program::program::invoke_signed;
use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;
use solana_program::rent::Rent;
use solana_program::sysvar::Sysvar;
use solana_program::{msg, system_instruction, system_program};

use crate::accounts::{GatewayConfig, GatewayExecuteData, GatewayMessageID};
use crate::check_program_account;
use crate::error::GatewayError;
use crate::events::{emit_call_contract_event, emit_operatorship_transferred_event};
use crate::instructions::GatewayInstruction;

/// Program state handler.
pub struct Processor;

impl Processor {
    /// Processes an instruction.
    pub fn process_instruction(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        input: &[u8],
    ) -> ProgramResult {
        let instruction = from_slice::<GatewayInstruction>(input)?;
        check_program_account(*program_id)?;
        match instruction {
            GatewayInstruction::Execute {} => {
                msg!("Instruction: Execute");
                Self::execute(accounts)
            }
            GatewayInstruction::CallContract {
                sender,
                destination_chain,
                destination_contract_address: destination_address,
                payload,
                payload_hash,
            } => {
                msg!("Instruction: Call Contract");
                emit_call_contract_event(
                    *sender,
                    destination_chain,
                    destination_address,
                    payload,
                    payload_hash,
                )?;
                Ok(())
            }
            GatewayInstruction::InitializeConfig { config } => {
                msg!("Instruction: Initialize Config");
                Self::initialize_config(accounts, &config)
            }
            GatewayInstruction::InitializeExecuteData { execute_data } => {
                msg!("Instruction: Initialize Execute Data");
                Self::initialize_execute_data(accounts, &execute_data)
            }
            GatewayInstruction::TransferOperatorship {} => {
                msg!("Instruction: TransferOperatorship");
                Self::transfer_operatorship(program_id, accounts)
            }
            GatewayInstruction::InitializeMessage { message_id } => {
                msg!("Instruction: Initialize Message ID");
                Self::initialize_message_id(accounts, &message_id)
            }
        }
    }

    fn execute(accounts: &[AccountInfo]) -> Result<(), ProgramError> {
        let mut accounts = accounts.iter();
        let gateway_config_account = next_account_info(&mut accounts)?;
        let execute_data_account = next_account_info(&mut accounts)?;
        let message_accounts: Vec<_> = accounts.collect();

        // Phase 1: Account validation

        // Check: Config account uses the canonical bump.
        let (canonical_pda, _canonical_bump) = crate::find_root_pda();
        if *gateway_config_account.key != canonical_pda {
            return Err(GatewayError::InvalidConfigAccount)?;
        }

        // Check: Config account is owned by the Gateway program.
        if *gateway_config_account.owner != crate::ID {
            return Err(ProgramError::InvalidAccountOwner);
        }

        // Check: Config account is read only.
        if gateway_config_account.is_writable {
            return Err(ProgramError::InvalidInstructionData);
        }

        // Check: execute_data account is owned by the Gateway program.
        if *execute_data_account.owner != crate::ID {
            return Err(ProgramError::InvalidAccountOwner);
        }

        // Check: execute_data account is writable
        if !execute_data_account.is_writable {
            return Err(ProgramError::InvalidInstructionData);
        }

        // Check: at least one message account
        if message_accounts.is_empty() {
            return Err(ProgramError::NotEnoughAccountKeys);
        }
        for message_account in message_accounts {
            // Check: All message accounts are writable.
            if !message_account.is_writable {
                return Err(ProgramError::InvalidInstructionData);
            }

            // Check: All message accounts are uninitialized
            if **message_account.lamports.borrow() > 0 {
                return Err(ProgramError::AccountAlreadyInitialized);
            }
        }

        // Phase 2: Deserialization
        solana_program::log::sol_log_compute_units();
        // Check: execute_data account was initialized
        let execute_data: GatewayExecuteData =
            borsh::from_slice(*execute_data_account.data.borrow())?;
        solana_program::log::sol_log_compute_units();
        let Ok((proof, decoded_commands)) = execute_data.decode() else {
            return Err(GatewayError::MalformedProof)?;
        };
        solana_program::log::sol_log_compute_units();
        proof.validate(todo!("message_hash (?)"));

        Ok(())
    }

    fn initialize_config(
        accounts: &[AccountInfo],
        gateway_config: &GatewayConfig,
    ) -> Result<(), ProgramError> {
        let accounts_iter = &mut accounts.iter();

        let payer = next_account_info(accounts_iter)?;
        let gateway_config_account = next_account_info(accounts_iter)?;
        let system_account = next_account_info(accounts_iter)?;

        // Check: System Program Account
        if !system_program::check_id(system_account.key) {
            return Err(GatewayError::InvalidSystemAccount.into());
        }

        // Check: Gateway Config account uses the canonical bump.
        let (canonical_pda, canonical_bump) = crate::find_root_pda();
        if *gateway_config_account.key != canonical_pda {
            return Err(GatewayError::InvalidConfigAccount.into());
        }

        init_pda(
            payer,
            gateway_config_account,
            &[&[canonical_bump]],
            gateway_config,
        )
    }

    fn initialize_execute_data(
        accounts: &[AccountInfo<'_>],
        execute_data: &GatewayExecuteData,
    ) -> Result<(), ProgramError> {
        let accounts_iter = &mut accounts.iter();
        let payer = next_account_info(accounts_iter)?;
        let execute_data_account = next_account_info(accounts_iter)?;
        let system_account = next_account_info(accounts_iter)?;

        // Check: System Program Account
        if !system_program::check_id(system_account.key) {
            return Err(GatewayError::InvalidSystemAccount.into());
        }

        // Check: Execute Data account uses the canonical bump.
        let (canonical_pda, bump, seeds) = execute_data.pda();
        if *execute_data_account.key != canonical_pda {
            return Err(GatewayError::InvalidExecuteDataAccount.into());
        }
        init_pda(
            payer,
            execute_data_account,
            &[seeds.as_ref(), &[bump]],
            execute_data,
        )
    }

    fn initialize_message_id(
        accounts: &[AccountInfo<'_>],
        message_id: &GatewayMessageID,
    ) -> Result<(), ProgramError> {
        let accounts_iter = &mut accounts.iter();

        let payer = next_account_info(accounts_iter)?;
        let message_id_account = next_account_info(accounts_iter)?;
        let system_account = next_account_info(accounts_iter)?;

        // Check: System Program Account
        if !system_program::check_id(system_account.key) {
            return Err(GatewayError::InvalidSystemAccount.into());
        }

        // Check: Message ID account uses the canonical bump.
        let (canonical_pda, bump, seeds) = message_id.pda();
        if *message_id_account.key != canonical_pda {
            return Err(GatewayError::InvalidMessageIDAccount.into());
        }
        init_pda(
            payer,
            message_id_account,
            &[seeds.as_ref(), &[bump]],
            message_id,
        )
    }

    fn transfer_operatorship(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
    ) -> Result<(), ProgramError> {
        check_program_account(*program_id)?;

        // Extract required accounts.
        let accounts_iter = &mut accounts.iter();
        let payer_account = next_account_info(accounts_iter)?;
        let new_operators_account = next_account_info(accounts_iter)?;
        let state_account = next_account_info(accounts_iter)?;
        let system_account = next_account_info(accounts_iter)?;

        // Check: state info account is the canonical PDA.
        let (expected_pda_info, _bump) = crate::find_root_pda();
        helper::compare_address(state_account, expected_pda_info)?;
        // Unpack the data from the new operators account.
        let new_operators_bytes: &[u8] = &new_operators_account.data.borrow();
        let new_operators =
            borsh::de::from_slice::<TransferOperatorshipAccount>(new_operators_bytes)?;

        // Check: new operator data is valid.
        if let Err(error) = new_operators.validate() {
            // TODO: Error handling should not be this brittle. Consider merging this part
            // of the AuthWeighted program into the Gateway.
            let gateway_error = match error {
                AuthWeightedError::InvalidOperators => GatewayError::InvalidOperators,
                AuthWeightedError::InvalidWeights => GatewayError::InvalidWeights,
                AuthWeightedError::InvalidThreshold => GatewayError::InvalidThreshold,
                _ => panic!("Unexpected error received from AuthWeighted module"),
            };
            return Err(gateway_error)?;
        };

        // Hash the new operator set.
        let new_operators_hash = hash(new_operators_bytes).to_bytes();

        // Unpack state data.
        let mut state: AuthWeightedStateAccount = {
            let state_bytes_ref = state_account.try_borrow_mut_data()?;
            borsh::de::from_slice(&state_bytes_ref)?
        };

        // Update epoch and operators.
        if let Err(error) = state.update_epoch_and_operators(new_operators_hash) {
            // TODO: Error handling should not be this brittle. Consider merging this part
            // of the AuthWeighted program into the Gateway.
            let gateway_error = match error {
                AuthWeightedError::DuplicateOperators => GatewayError::DuplicateOperators,
                _ => panic!("Unexpected error received from AuthWeighted module"),
            };
            return Err(gateway_error)?;
        };

        // Resize and refund state account space.
        state.reallocate(state_account, payer_account, system_account)?;

        // Write the packed data back to the state account.
        let serialized_state = borsh::to_vec(&state)?;
        let mut state_data_ref = state_account.try_borrow_mut_data()?;
        state_data_ref[..serialized_state.len()].copy_from_slice(&serialized_state);

        // Emit an event to signal the successful operatorship transfer
        emit_operatorship_transferred_event(*new_operators_account.key)?;
        Ok(())
    }
}

/// Initialize a Gateway PDA
fn init_pda<'a, 'b, T: borsh::BorshSerialize>(
    payer: &'a AccountInfo<'b>,
    new_account_pda: &'a AccountInfo<'b>,
    seeds: &[&[u8]],
    data: &T,
) -> Result<(), ProgramError> {
    let serialized_data = borsh::to_vec(data)?;
    let space = serialized_data.len();
    let rent_sysvar = Rent::get()?;
    let rent = rent_sysvar.minimum_balance(space);

    assert!(payer.is_signer);
    assert!(payer.is_writable);
    // Note that `new_account_pda` is not a signer yet.
    // This program will sign for it via `invoke_signed`.
    assert!(!new_account_pda.is_signer);
    assert!(new_account_pda.is_writable);
    assert_eq!(new_account_pda.owner, &system_program::ID);

    invoke_signed(
        &system_instruction::create_account(
            payer.key,
            new_account_pda.key,
            rent,
            space
                .try_into()
                .map_err(|_| ProgramError::ArithmeticOverflow)?,
            &crate::ID,
        ),
        &[payer.clone(), new_account_pda.clone()],
        &[seeds],
    )?;
    let mut account_data = new_account_pda.try_borrow_mut_data()?;
    account_data[..space].copy_from_slice(&serialized_data);
    Ok(())
}

mod helper {
    use solana_program::account_info::AccountInfo;
    use solana_program::pubkey::Pubkey;

    use crate::error::GatewayError;

    /// Compares the account address with the expected address.
    pub(super) fn compare_address(
        pda_info: &AccountInfo<'_>,
        expected_pda_info: Pubkey,
    ) -> Result<(), GatewayError> {
        if pda_info.key != &expected_pda_info {
            Err(GatewayError::IncorrectAccountAddr)
        } else {
            Ok(())
        }
    }
}
