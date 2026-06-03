#![no_std]
//! xete-tab — non-custodial confidential settlement, Pinocchio (lean) build.
//!
//! This is the build deployed on-chain: no_std, zero heap, manual
//! (de)serialization, minimal compute. It shares an identical wire format and
//! PDA derivation with the human-readable `solana-program` reference under
//! `program/readable/`, so the same client and test suite drive both. A
//! line-by-line translation ships in TRANSLATION.md.
//!
//! A depositor funds a program-owned PDA naming a HIDDEN beneficiary (a hash
//! commitment, never a pubkey). The beneficiary learns the salt off-chain
//! (encrypted, over xete) and claims by proving `H(pubkey || salt) == commitment`.
//! On claim the account closes and its whole balance — principal *and* rent
//! reserve — goes to the beneficiary. The depositor may reclaim until then.
//!
//! Non-custodial: only the depositor's and beneficiary's own keys move funds;
//! the program can never release on its own. Not a mixer: each settlement is a
//! discrete, on-chain A->PDA->B account and the deposit<->claim link is
//! permanent and public — only the beneficiary is concealed, and only until claim.

use pinocchio::{
    account::AccountView,
    address::Address,
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;
use solana_nostd_sha256::hashv;

// #![no_std] program: wire the entrypoint, allocator, and the no_std panic
// lang item individually (the all-in-one `entrypoint!` assumes std provides
// the panic handler).
pinocchio::program_entrypoint!(process);
pinocchio::default_allocator!();
pinocchio::nostd_panic_handler!();

// PDA seed. NOTE: the on-chain seed is the literal b"escrow" — a fixed internal
// constant from this contract's first deployment. It is load-bearing (the live,
// immutable program derives every account from it), so it cannot change. The
// protocol and product are "settlement"; this is the one place an older name
// survives. Any client must use this exact seed. See SPEC.md.
const SEED: &[u8] = b"escrow";

// On-chain state layout (81 bytes), written/read by hand.
const O_DEPOSITOR: usize = 0; // [u8;32]
const O_AMOUNT: usize = 32; //   u64 LE
const O_COMMIT: usize = 40; //   [u8;32]
const O_UNLOCK: usize = 72; //   i64 LE
const O_BUMP: usize = 80; //     u8
const STATE_LEN: usize = 81;

fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    match tag {
        0 => deposit(program_id, accounts, rest),
        1 => claim(program_id, accounts, rest),
        2 => reclaim(program_id, accounts, rest),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn arr32(b: &[u8]) -> Result<[u8; 32], ProgramError> {
    b.try_into().map_err(|_| ProgramError::InvalidInstructionData)
}

fn deposit(program_id: &Address, accounts: &mut [AccountView], d: &[u8]) -> ProgramResult {
    if d.len() != 80 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let settlement_id = arr32(&d[0..32])?;
    let amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let commitment = arr32(&d[40..72])?;
    let unlock = i64::from_le_bytes(d[72..80].try_into().unwrap());

    let [depositor, settlement, _system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !depositor.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let (pda, bump) = Address::find_program_address(&[SEED, &settlement_id], program_id);
    if &pda != settlement.address() {
        return Err(ProgramError::InvalidSeeds);
    }
    // Rent floor (the minimum send) is enforced by create_account itself.

    let bump_seed = [bump];
    let seeds = [
        Seed::from(SEED),
        Seed::from(settlement_id.as_ref()),
        Seed::from(bump_seed.as_ref()),
    ];
    CreateAccount {
        from: &*depositor,
        to: &*settlement,
        lamports: amount,
        space: STATE_LEN as u64,
        owner: program_id,
    }
    .invoke_signed(&[Signer::from(&seeds[..])])?;

    let mut s = settlement.try_borrow_mut()?;
    s[O_DEPOSITOR..O_DEPOSITOR + 32].copy_from_slice(depositor.address().as_ref());
    s[O_AMOUNT..O_AMOUNT + 8].copy_from_slice(&amount.to_le_bytes());
    s[O_COMMIT..O_COMMIT + 32].copy_from_slice(&commitment);
    s[O_UNLOCK..O_UNLOCK + 8].copy_from_slice(&unlock.to_le_bytes());
    s[O_BUMP] = bump;
    Ok(())
}

fn claim(program_id: &Address, accounts: &mut [AccountView], d: &[u8]) -> ProgramResult {
    if d.len() < 36 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let settlement_id = arr32(&d[0..32])?;
    let salt_len = u32::from_le_bytes(d[32..36].try_into().unwrap()) as usize;
    let salt = d.get(36..36 + salt_len).ok_or(ProgramError::InvalidInstructionData)?;

    let [beneficiary, settlement] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !beneficiary.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    verify_pda(program_id, settlement, &settlement_id)?;

    let unlock = {
        let s = settlement.try_borrow()?;
        // Prove the claimant is the hidden beneficiary: H(pubkey || salt) == commitment.
        let stored = arr32(&s[O_COMMIT..O_COMMIT + 32])?;
        if hashv(&[beneficiary.address().as_ref(), salt]) != stored {
            return Err(ProgramError::InvalidArgument);
        }
        i64::from_le_bytes(s[O_UNLOCK..O_UNLOCK + 8].try_into().unwrap())
    };
    if Clock::get()?.unix_timestamp < unlock {
        return Err(ProgramError::InvalidArgument);
    }

    close(settlement, beneficiary)
}

fn reclaim(program_id: &Address, accounts: &mut [AccountView], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let settlement_id = arr32(&d[0..32])?;

    let [depositor, settlement] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !depositor.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    verify_pda(program_id, settlement, &settlement_id)?;

    {
        let s = settlement.try_borrow()?;
        if &s[O_DEPOSITOR..O_DEPOSITOR + 32] != depositor.address().as_ref() {
            return Err(ProgramError::IllegalOwner);
        }
    }
    close(settlement, depositor)
}

fn verify_pda(program_id: &Address, settlement: &AccountView, settlement_id: &[u8; 32]) -> ProgramResult {
    let (pda, _) = Address::find_program_address(&[SEED, settlement_id], program_id);
    if &pda != settlement.address() {
        return Err(ProgramError::InvalidSeeds);
    }
    if settlement.owner() != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    Ok(())
}

/// Sweep every lamport (principal + rent) into `dest` and wipe the data.
fn close(account: &mut AccountView, dest: &mut AccountView) -> ProgramResult {
    let amt = account.lamports();
    let new_dest = dest.lamports().checked_add(amt).ok_or(ProgramError::ArithmeticOverflow)?;
    dest.set_lamports(new_dest);
    account.set_lamports(0);
    account.try_borrow_mut()?.fill(0);
    Ok(())
}
