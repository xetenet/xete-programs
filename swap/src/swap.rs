//! 1:1 SPL/Token-2022 swap: lock a give, post a want, fill atomically (tags 0-3).

use crate::cpi::*;
use crate::config::*;
use crate::settle::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

// ---- 3. open_swap (tag 0) ----
// data: nonce[32] | give_amount u64 | want_amount u64 | terms[32] | expiry i64 | taker[32]  (120; taker all-zero = open)
// accounts: maker(signer) | swap(pda) | vault(pda) | give_mint | want_mint
//           | maker_give_ata | token_program | system | config | alias
pub(crate) fn open_swap(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 120 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[40..48].try_into().unwrap());
    let terms = arr32(&d[48..80])?;
    let expiry = i64::from_le_bytes(d[80..88].try_into().unwrap());
    let target_taker = arr32(&d[88..120])?; // all-zero = open to any taker

    let [maker, swap, vault, give_mint, want_mint, maker_give, token_prog, system, config, alias] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if give_amount == 0 || want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    // listing gate: maker must hold a registered alias (no-op if the gate is off in config)
    require_alias(program_id, config, maker.key.as_ref(), alias)?;

    let (swap_pda, bump) = Pubkey::find_program_address(&[b"swap", maker.key.as_ref(), &nonce], program_id);
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (vault_pda, vbump) = Pubkey::find_program_address(&[b"vault", maker.key.as_ref(), &nonce], program_id);
    if &vault_pda != vault.key {
        return Err(ProgramError::InvalidSeeds);
    }

    // 1. create the swap state account (PDA-signed)
    let bs = [bump];
    create_pda(maker, swap, system, SWAP_LEN, program_id, &[b"swap", maker.key.as_ref(), &nonce, &bs])?;
    crate::events::emit_post_created(crate::events::KIND_SWAP, maker.key, swap.key, give_mint.key, want_mint.key, give_amount, want_amount, expiry, &nonce);
    // 2. create the vault token account (PDA-signed; owned by the Token program)
    let vbs = [vbump];
    assert_token_program(token_prog, give_mint)?;
    create_pda(maker, vault, system, TOKEN_ACCT_LEN, token_prog.key, &[b"vault", maker.key.as_ref(), &nonce, &vbs])?;
    // 3. initialize the vault; its token authority is the swap PDA
    token_init(token_prog, vault, give_mint, swap.key)?;
    // 4. pull the give-leg into the vault (decimals read straight from the mint)
    token_transfer(token_prog, maker_give, give_mint, vault, maker, give_amount, mint_decimals(give_mint)?, None)?;

    // 5. write swap state
    let mut s = swap.try_borrow_mut_data()?;
    s[S_MAKER..S_MAKER + 32].copy_from_slice(maker.key.as_ref());
    s[S_GIVE_MINT..S_GIVE_MINT + 32].copy_from_slice(give_mint.key.as_ref());
    s[S_GIVE_AMT..S_GIVE_AMT + 8].copy_from_slice(&give_amount.to_le_bytes());
    s[S_WANT_MINT..S_WANT_MINT + 32].copy_from_slice(want_mint.key.as_ref());
    s[S_WANT_AMT..S_WANT_AMT + 8].copy_from_slice(&want_amount.to_le_bytes());
    s[S_TAKER..S_TAKER + 32].copy_from_slice(&target_taker); // all-zero = open to any taker
    s[S_TERMS..S_TERMS + 32].copy_from_slice(&terms);
    s[S_EXPIRY..S_EXPIRY + 8].copy_from_slice(&expiry.to_le_bytes());
    s[S_STATUS] = STATUS_OPEN;
    s[S_BUMP] = bump;
    s[S_NONCE..S_NONCE + 32].copy_from_slice(&nonce);
    Ok(())
}

// ---- 4. fill (tag 1) — atomic two-leg settle at the taker's agreed price ----
// data: nonce[32] | expect_give u64 | max_want u64  (48)
// accounts: taker(signer) | swap(pda) | vault(pda) | maker | give_mint | want_mint
//           | taker_want_ata | taker_give_ata | maker_want_ata | token_program | config | fee_ata
pub(crate) fn fill(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let expect_give = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let max_want = u64::from_le_bytes(d[40..48].try_into().unwrap());

    if accounts.len() < 12 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let [taker, swap, vault, maker, give_mint, want_mint, taker_want, taker_give, maker_want, token_prog, config, fee_ata] =
        &accounts[..12]
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    // give-leg token program: optional trailing account; defaults to the want program (classic single-program swaps)
    let give_token_prog = accounts.get(12).unwrap_or(token_prog);
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    // load + validate swap state (borrow dropped at block end)
    let (state_maker, give_amount, want_amount, bump, _royalty_mode, _maker_pct) = load_fill_state(swap, give_mint.key, want_mint.key, taker.key)?;

    // slippage guard: the taker pins the exact terms they agreed to
    if give_amount != expect_give || want_amount > max_want {
        return Err(ProgramError::InvalidArgument);
    }
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let swap_pda = Pubkey::create_program_address(&[b"swap", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault", &state_maker, &nonce], program_id);
    if &vault_pda != vault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    // the payment must land in the MAKER's want-mint account (else a taker could redirect it)
    require_token_account(maker_want, want_mint.key, &state_maker)?;

    let give_dec = mint_decimals(give_mint)?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(token_prog, want_mint)?;        // want leg program
    assert_token_program(give_token_prog, give_mint)?;   // give leg (vault) program

    // leg 1: taker pays the want-leg; the protocol fee is skimmed to the fee wallet, maker gets the rest
    pay_want_leg(program_id, config, token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, want_dec, nonce[0])?;

    // leg 2: vault releases the give-leg to the taker (swap PDA signs)
    let bs = [bump];
    let swap_seeds: &[&[u8]] = &[b"swap", state_maker.as_ref(), &nonce, &bs];
    token_transfer(give_token_prog, vault, give_mint, taker_give, swap, give_amount, give_dec, Some(swap_seeds))?;

    // close the now-empty vault (rent -> maker), then the swap state (rent -> maker)
    token_close(give_token_prog, vault, maker, swap, swap_seeds)?;
    close(swap, maker)
}

// ---- 5. cancel_swap (tag 2) — maker reclaims the give-leg before any fill ----
// data: nonce[32]
// accounts: maker(signer) | swap(pda) | vault(pda) | give_mint | maker_give_ata | token_program | config | fee_wallet | system
pub(crate) fn cancel_swap(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;

    let [maker, swap, vault, give_mint, maker_give, token_prog, config, fee_wallet, system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (give_amount, bump) = {
        let s = swap.try_borrow_data()?;
        if s.len() != SWAP_LEN || s[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &s[S_MAKER..S_MAKER + 32] != maker.key.as_ref() {
            return Err(ProgramError::IllegalOwner); // only the maker may cancel
        }
        if &s[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        (u64::from_le_bytes(s[S_GIVE_AMT..S_GIVE_AMT + 8].try_into().unwrap()), s[S_BUMP])
    };
    // delist fee: a flat lamport charge to the fee wallet (anti-churn); rent still refunds to the maker
    let (_bps, delist_fee, cfg_wallet, _shards) = read_config(program_id, config)?;
    if delist_fee > 0 {
        if fee_wallet.key.as_ref() != cfg_wallet.as_ref() {
            return Err(ProgramError::InvalidArgument); // must be the configured fee wallet
        }
        invoke(
            &system_instruction::transfer(maker.key, fee_wallet.key, delist_fee),
            &[maker.clone(), fee_wallet.clone(), system.clone()],
        )?;
    }
    refund_and_close(program_id, swap, vault, maker, give_mint, maker_give, token_prog, &nonce, bump, give_amount)
}

// ---- 6. expire (tag 3) — permissionless refund to the maker after the deadline ----
// data: nonce[32]
// accounts: caller(signer/payer) | swap(pda) | vault(pda) | maker | give_mint | maker_give_ata | token_program
pub(crate) fn expire(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;

    let [_caller, swap, vault, maker, give_mint, maker_give, token_prog] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (state_maker, give_amount, bump) = {
        let s = swap.try_borrow_data()?;
        if s.len() != SWAP_LEN || s[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &s[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        let expiry = i64::from_le_bytes(s[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < expiry {
            return Err(ProgramError::InvalidArgument); // not expired yet
        }
        (
            arr32(&s[S_MAKER..S_MAKER + 32])?,
            u64::from_le_bytes(s[S_GIVE_AMT..S_GIVE_AMT + 8].try_into().unwrap()),
            s[S_BUMP],
        )
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    refund_and_close(program_id, swap, vault, maker, give_mint, maker_give, token_prog, &nonce, bump, give_amount)
}

/// Refund the escrowed give-leg to the maker, then close vault + state (rent -> maker).
/// Shared by cancel_swap and expire. `maker_give` is verified to be the MAKER's give account,
/// so the permissionless `expire` path can't redirect the refund.
#[allow(clippy::too_many_arguments)]
pub(crate) fn refund_and_close<'a>(
    program_id: &Pubkey,
    swap: &AccountInfo<'a>,
    vault: &AccountInfo<'a>,
    maker: &AccountInfo<'a>,
    give_mint: &AccountInfo<'a>,
    maker_give: &AccountInfo<'a>,
    token_prog: &AccountInfo<'a>,
    nonce: &[u8; 32],
    bump: u8,
    give_amount: u64,
) -> ProgramResult {
    let mk = *maker.key;
    require_token_account(maker_give, give_mint.key, mk.as_ref())?;
    let swap_pda = Pubkey::create_program_address(&[b"swap", mk.as_ref(), nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault", mk.as_ref(), nonce], program_id);
    if &vault_pda != vault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let give_dec = mint_decimals(give_mint)?;
    let bs = [bump];
    let seeds: &[&[u8]] = &[b"swap", mk.as_ref(), nonce, &bs];
    token_transfer(token_prog, vault, give_mint, maker_give, swap, give_amount, give_dec, Some(seeds))?;
    token_close(token_prog, vault, maker, swap, seeds)?;
    close(swap, maker)
}
