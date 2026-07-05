//! Metaplex Core asset escrow swap (tags 22-25).

use crate::cpi::*;
use crate::config::*;
use crate::settle::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

// ---- open_core (tag 22) -- escrow a Core asset by transferring ownership to the swap PDA ----
// data: nonce[32] give_amount(=1) want_amount terms[32] expiry taker[32]  (120)
// accounts: maker(s) | swap(pda,w) | asset(w) | collection | want_mint | mpl_core | system | config | alias
pub(crate) fn open_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 120 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[40..48].try_into().unwrap());
    let terms = arr32(&d[48..80])?;
    let expiry = i64::from_le_bytes(d[80..88].try_into().unwrap());
    let target_taker = arr32(&d[88..120])?;

    let [maker, swap, asset, collection, want_mint, mpl_core, system, config, alias] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if give_amount != 1 || want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    require_alias(program_id, config, maker.key.as_ref(), alias)?;

    let (swap_pda, bump) = Pubkey::find_program_address(&[b"swap", maker.key.as_ref(), &nonce], program_id);
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [bump];
    create_pda(maker, swap, system, SWAP_LEN, program_id, &[b"swap", maker.key.as_ref(), &nonce, &bs])?;
    crate::events::emit_post_created(crate::events::KIND_SWAP_CORE, maker.key, swap.key, asset.key, want_mint.key, give_amount, want_amount, expiry, &nonce);
    // CPI Transfer: maker (owner) -> swap PDA. authority = sentinel(core) -> mpl-core uses payer(maker) as authority.
    mpl_core_transfer(mpl_core, asset, collection, maker, mpl_core, swap, false, None)?;
    // state (asset pubkey stored in the give-mint slot; give_amount = 1)
    let mut st = swap.try_borrow_mut_data()?;
    st[S_MAKER..S_MAKER + 32].copy_from_slice(maker.key.as_ref());
    st[S_GIVE_MINT..S_GIVE_MINT + 32].copy_from_slice(asset.key.as_ref());
    st[S_GIVE_AMT..S_GIVE_AMT + 8].copy_from_slice(&1u64.to_le_bytes());
    st[S_WANT_MINT..S_WANT_MINT + 32].copy_from_slice(want_mint.key.as_ref());
    st[S_WANT_AMT..S_WANT_AMT + 8].copy_from_slice(&want_amount.to_le_bytes());
    st[S_TAKER..S_TAKER + 32].copy_from_slice(&target_taker);
    st[S_TERMS..S_TERMS + 32].copy_from_slice(&terms);
    st[S_EXPIRY..S_EXPIRY + 8].copy_from_slice(&expiry.to_le_bytes());
    st[S_STATUS] = STATUS_OPEN;
    st[S_BUMP] = bump;
    st[S_NONCE..S_NONCE + 32].copy_from_slice(&nonce);
    Ok(())
}

/// Release an escrowed Core asset out of the swap PDA to `new_owner` (the swap PDA signs as authority).
#[allow(clippy::too_many_arguments)]
pub(crate) fn release_core<'a>(
    core: &AccountInfo<'a>, asset: &AccountInfo<'a>, collection: &AccountInfo<'a>, swap: &AccountInfo<'a>,
    payer: &AccountInfo<'a>, new_owner: &AccountInfo<'a>, state_maker: &[u8], nonce: &[u8], bump: u8,
) -> ProgramResult {
    let bs = [bump];
    let seeds: &[&[u8]] = &[b"swap", state_maker, nonce, &bs];
    mpl_core_transfer(core, asset, collection, payer, swap, new_owner, true, Some(seeds))
}

// ---- fill_core (tag 23) -- taker pays the want-leg; the swap PDA transfers the Core asset to the taker ----
// data: nonce[32] expect_give u64 max_want u64  (48)
// accounts: taker(s) | swap(pda,w) | maker(w) | asset(w) | collection | want_mint | taker_want(w) | maker_want(w)
//   | want_token_prog | config | fee_ata(w) | mpl_core
pub(crate) fn fill_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let expect_give = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let max_want = u64::from_le_bytes(d[40..48].try_into().unwrap());

    let [taker, swap, maker, asset, collection, want_mint, taker_want, maker_want, want_token_prog, config, fee_ata, mpl_core] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (state_maker, give_amount, want_amount, bump, _royalty_mode, _maker_pct) = load_fill_state(swap, asset.key, want_mint.key, taker.key)?;
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
    require_token_account(maker_want, want_mint.key, &state_maker)?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(want_token_prog, want_mint)?;

    // leg 1: want payment (fee skim + remainder to maker)
    pay_want_leg(program_id, config, want_token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, want_dec, nonce[0])?;

    // leg 2: the swap PDA transfers the Core asset to the taker
    release_core(mpl_core, asset, collection, swap, taker, taker, &state_maker, &nonce, bump)?;
    close(swap, maker)
}

// ---- cancel_core (tag 24) -- maker reclaims the escrowed Core asset before any fill ----
// data: nonce[32]
// accounts: maker(s) | swap(pda,w) | asset(w) | collection | config | fee_wallet(w) | system | mpl_core
pub(crate) fn cancel_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [maker, swap, asset, collection, config, fee_wallet, system, mpl_core] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let bump = {
        let st = swap.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &st[S_MAKER..S_MAKER + 32] != maker.key.as_ref() {
            return Err(ProgramError::IllegalOwner);
        }
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != asset.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        st[S_BUMP]
    };
    let swap_pda = Pubkey::create_program_address(&[b"swap", maker.key.as_ref(), &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (_bps, delist_fee, cfg_wallet, _shards) = read_config(program_id, config)?;
    if delist_fee > 0 {
        if fee_wallet.key.as_ref() != cfg_wallet.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        invoke(
            &system_instruction::transfer(maker.key, fee_wallet.key, delist_fee),
            &[maker.clone(), fee_wallet.clone(), system.clone()],
        )?;
    }
    release_core(mpl_core, asset, collection, swap, maker, maker, maker.key.as_ref(), &nonce, bump)?;
    close(swap, maker)
}

// ---- expire_core (tag 25) -- permissionless refund of the Core asset to the maker after the deadline ----
// data: nonce[32]
// accounts: caller(s) | swap(pda,w) | maker(w) | asset(w) | collection | mpl_core
pub(crate) fn expire_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [caller, swap, maker, asset, collection, mpl_core] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (state_maker, bump) = {
        let st = swap.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != asset.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        let expiry = i64::from_le_bytes(st[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < expiry {
            return Err(ProgramError::InvalidArgument);
        }
        (arr32(&st[S_MAKER..S_MAKER + 32])?, st[S_BUMP])
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let swap_pda = Pubkey::create_program_address(&[b"swap", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    release_core(mpl_core, asset, collection, swap, caller, maker, &state_maker, &nonce, bump)?;
    close(swap, maker)
}
