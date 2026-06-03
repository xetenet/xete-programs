//! xete-tab — non-custodial confidential settlement (human-readable reference).
//!
//! This `solana-program` build is the easy-to-audit reference for the lean
//! Pinocchio build under `program/lean/` (which is what runs on-chain). Both
//! share an identical wire format and PDA derivation, so the same client and
//! test suite drive both. See TRANSLATION.md for a line-by-line mapping.
//!
//! A depositor funds a program-owned PDA that names a HIDDEN beneficiary
//! (a hash commitment, never a pubkey). The beneficiary learns the salt
//! off-chain (encrypted, over xete) and claims by proving
//! `H(B_pubkey || salt) == commitment`. On claim the account is closed and its
//! whole balance — principal *and* rent reserve — goes to B. A may reclaim
//! until then.
//!
//! Non-custodial: only the depositor's and beneficiary's own keys move funds;
//! the program can never release on its own. Not a mixer: each settlement is a
//! discrete, on-chain A->PDA->B account and the deposit<->claim link is
//! permanent and public — only the beneficiary is concealed, and only until
//! claim. The existence, amount, and depositor are always visible.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    hash::hashv,
    msg,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::{clock::Clock, Sysvar},
};
use solana_system_interface::instruction as system_instruction;

// PDA seed. NOTE: the on-chain seed is the literal b"escrow" — a fixed internal
// constant from this contract's first deployment. It is load-bearing (the live,
// immutable program derives every account from it), so it cannot change. The
// protocol and product are "settlement"; this is the one place an older name
// survives. Any client must use this exact seed. See SPEC.md.
const SEED: &[u8] = b"escrow";

/// Settlement account state — fixed-size, borsh-encoded. No manual byte offsets.
#[derive(BorshSerialize, BorshDeserialize)]
pub struct Settlement {
    pub depositor: Pubkey,                 // funded it; the only key that may reclaim
    pub amount: u64,                       // lamports deposited (for the receipt)
    pub beneficiary_commitment: [u8; 32],  // H(beneficiary_pubkey || salt)
    pub unlock_time: i64,                  // unix secs; 0 = claimable immediately
    pub bump: u8,
}
impl Settlement {
    const LEN: usize = 32 + 8 + 32 + 8 + 1; // 81 bytes
}

/// `settlement_id` is a caller-chosen random 32-byte id — NOT the beneficiary —
/// so the PDA address itself never reveals who the funds are for.
#[derive(BorshSerialize, BorshDeserialize)]
pub enum SettlementInstruction {
    /// accounts: [signer, w] depositor, [w] settlement_pda, [] system_program
    Deposit {
        settlement_id: [u8; 32],
        amount: u64,
        beneficiary_commitment: [u8; 32],
        unlock_time: i64,
    },
    /// accounts: [signer, w] beneficiary, [w] settlement_pda
    Claim { settlement_id: [u8; 32], salt: Vec<u8> },
    /// accounts: [signer, w] depositor, [w] settlement_pda
    Reclaim { settlement_id: [u8; 32] },
}

entrypoint!(process);

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    match SettlementInstruction::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)? {
        SettlementInstruction::Deposit { settlement_id, amount, beneficiary_commitment, unlock_time } => {
            deposit(program_id, accounts, settlement_id, amount, beneficiary_commitment, unlock_time)
        }
        SettlementInstruction::Claim { settlement_id, salt } => claim(program_id, accounts, settlement_id, salt),
        SettlementInstruction::Reclaim { settlement_id } => reclaim(program_id, accounts, settlement_id),
    }
}

fn settlement_pda(program_id: &Pubkey, settlement_id: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED, settlement_id], program_id)
}

/// Load + verify the settlement PDA: right address, owned by us, present.
fn load(program_id: &Pubkey, settlement: &AccountInfo, settlement_id: &[u8; 32]) -> Result<Settlement, ProgramError> {
    let (pda, _) = settlement_pda(program_id, settlement_id);
    if pda != *settlement.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if settlement.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    Settlement::try_from_slice(&settlement.data.borrow()).map_err(|_| ProgramError::InvalidAccountData)
}

/// Close `account` into `dest`: sweep every lamport (principal + rent) and wipe data.
fn close(account: &AccountInfo, dest: &AccountInfo) -> ProgramResult {
    let lamports = account.lamports();
    **dest.try_borrow_mut_lamports()? = dest
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **account.try_borrow_mut_lamports()? = 0;
    account.try_borrow_mut_data()?.fill(0);
    Ok(())
}

fn deposit(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    settlement_id: [u8; 32],
    amount: u64,
    beneficiary_commitment: [u8; 32],
    unlock_time: i64,
) -> ProgramResult {
    let it = &mut accounts.iter();
    let depositor = next_account_info(it)?;
    let settlement = next_account_info(it)?;
    let system_program = next_account_info(it)?;

    if !depositor.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (pda, bump) = settlement_pda(program_id, &settlement_id);
    if pda != *settlement.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if settlement.lamports() > 0 || settlement.data_len() > 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    // The rent reserve is borrowed from the send: the account is funded with the
    // full `amount`, which must clear rent-exemption. That floor IS the minimum send.
    let rent_min = Rent::get()?.minimum_balance(Settlement::LEN);
    if amount < rent_min {
        msg!("xete-settle: amount {} below rent floor {}", amount, rent_min);
        return Err(ProgramError::InsufficientFunds);
    }

    invoke_signed(
        &system_instruction::create_account(
            depositor.key,
            settlement.key,
            amount,
            Settlement::LEN as u64,
            program_id,
        ),
        &[depositor.clone(), settlement.clone(), system_program.clone()],
        &[&[SEED, &settlement_id, &[bump]]],
    )?;

    let state = Settlement { depositor: *depositor.key, amount, beneficiary_commitment, unlock_time, bump };
    let bytes = borsh::to_vec(&state)?;
    settlement.data.borrow_mut()[..bytes.len()].copy_from_slice(&bytes);

    msg!("XETE_SETTLE_OPEN pda={} from={} amount={}", settlement.key, depositor.key, amount);
    Ok(())
}

fn claim(program_id: &Pubkey, accounts: &[AccountInfo], settlement_id: [u8; 32], salt: Vec<u8>) -> ProgramResult {
    let it = &mut accounts.iter();
    let beneficiary = next_account_info(it)?;
    let settlement = next_account_info(it)?;

    if !beneficiary.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let state = load(program_id, settlement, &settlement_id)?;

    // Prove the claimant is the hidden beneficiary: H(claimant_pubkey || salt) == commitment.
    let computed = hashv(&[beneficiary.key.as_ref(), &salt]).to_bytes();
    if computed != state.beneficiary_commitment {
        msg!("xete-settle: commitment mismatch");
        return Err(ProgramError::InvalidArgument);
    }
    if Clock::get()?.unix_timestamp < state.unlock_time {
        msg!("xete-settle: locked until {}", state.unlock_time);
        return Err(ProgramError::InvalidArgument);
    }

    close(settlement, beneficiary)?;
    msg!("XETE_SETTLE_CLAIM pda={} to={} amount={}", settlement.key, beneficiary.key, state.amount);
    Ok(())
}

fn reclaim(program_id: &Pubkey, accounts: &[AccountInfo], settlement_id: [u8; 32]) -> ProgramResult {
    let it = &mut accounts.iter();
    let depositor = next_account_info(it)?;
    let settlement = next_account_info(it)?;

    if !depositor.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let state = load(program_id, settlement, &settlement_id)?;
    if state.depositor != *depositor.key {
        return Err(ProgramError::IllegalOwner);
    }

    close(settlement, depositor)?;
    msg!("XETE_SETTLE_RECLAIM pda={} to={} amount={}", settlement.key, depositor.key, state.amount);
    Ok(())
}
