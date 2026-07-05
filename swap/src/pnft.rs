//! Programmable-NFT (Token-Metadata) escrow swap (tags 18-21).

use crate::cpi::*;
use crate::config::*;
use crate::settle::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

// ---- open_pnft (tag 18) -- escrow a programmable NFT into the swap PDA's ATA via Token-Metadata Transfer ----
// data: nonce[32] give_amount(=1) want_amount terms[32] expiry taker[32]  (120, same shape as open_swap)
// accounts: maker(s) | swap(pda,w) | give_mint | want_mint | source_ata(w) | vault_ata(w) | metadata(w)
//   | edition | owner_record(w) | dest_record(w) | token_metadata | spl_token | ata_prog | sysvar_ix
//   | system | rules_prog | rules | config | alias
pub(crate) fn open_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 120 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[40..48].try_into().unwrap());
    let terms = arr32(&d[48..80])?;
    let expiry = i64::from_le_bytes(d[80..88].try_into().unwrap());
    let target_taker = arr32(&d[88..120])?;

    let [maker, swap, give_mint, want_mint, source_ata, vault_ata, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, sysvar_ix, system, rules_prog, rules, config, alias] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if give_amount != 1 || want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData); // an NFT is exactly one
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    require_alias(program_id, config, maker.key.as_ref(), alias)?;

    let (swap_pda, bump) = Pubkey::find_program_address(&[b"swap", maker.key.as_ref(), &nonce], program_id);
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    // the NFT must move FROM the maker's own ATA INTO the swap PDA's ATA -- no redirection either side
    if *source_ata.key != ata_for(maker.key, give_mint.key) || *vault_ata.key != ata_for(&swap_pda, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
    }

    // 1. create the swap state account (PDA-signed)
    let bs = [bump];
    create_pda(maker, swap, system, SWAP_LEN, program_id, &[b"swap", maker.key.as_ref(), &nonce, &bs])?;
    crate::events::emit_post_created(crate::events::KIND_SWAP_PNFT, maker.key, swap.key, give_mint.key, want_mint.key, give_amount, want_amount, expiry, &nonce);
    // 2. CPI Transfer: maker authorizes moving the pNFT into the swap PDA's ATA (the CPI creates the dest ATA + record)
    mpl_transfer(
        token_metadata, source_ata, maker, vault_ata, swap, give_mint, metadata, edition, owner_record,
        dest_record, maker, maker, system, sysvar_ix, spl_tok, ata_prog, rules_prog, rules, 1, None,
    )?;
    // 3. write swap state (same layout as open_swap; the release path signs as the swap PDA)
    let mut st = swap.try_borrow_mut_data()?;
    st[S_MAKER..S_MAKER + 32].copy_from_slice(maker.key.as_ref());
    st[S_GIVE_MINT..S_GIVE_MINT + 32].copy_from_slice(give_mint.key.as_ref());
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

/// Release an escrowed pNFT out of the swap PDA's vault ATA to `dst_ta`/`dst_owner`, the swap PDA
/// signing the Token-Metadata Transfer. `payer` funds the destination token account + record.
#[allow(clippy::too_many_arguments)]
pub(crate) fn release_pnft<'a>(
    tm: &AccountInfo<'a>, vault: &AccountInfo<'a>, swap: &AccountInfo<'a>, dst_ta: &AccountInfo<'a>,
    dst_owner: &AccountInfo<'a>, give_mint: &AccountInfo<'a>, metadata: &AccountInfo<'a>,
    edition: &AccountInfo<'a>, vault_record: &AccountInfo<'a>, dst_record: &AccountInfo<'a>,
    payer: &AccountInfo<'a>, system: &AccountInfo<'a>, sysvar_ix: &AccountInfo<'a>,
    spl_tok: &AccountInfo<'a>, ata_prog: &AccountInfo<'a>, rules_prog: &AccountInfo<'a>,
    rules: &AccountInfo<'a>, state_maker: &[u8], nonce: &[u8], bump: u8,
) -> ProgramResult {
    let bs = [bump];
    let seeds: &[&[u8]] = &[b"swap", state_maker, nonce, &bs];
    mpl_transfer(
        tm, vault, swap, dst_ta, dst_owner, give_mint, metadata, edition, vault_record, dst_record,
        swap, payer, system, sysvar_ix, spl_tok, ata_prog, rules_prog, rules, 1, Some(seeds),
    )
}

// ---- fill_pnft (tag 19) -- taker pays the want-leg; the vault releases the pNFT to the taker ----
// data: nonce[32] expect_give u64 max_want u64  (48, same as fill)
// accounts: taker(s) | swap(pda,w) | maker(w) | give_mint | want_mint | taker_want(w) | maker_want(w)
//   | want_token_prog | config | fee_ata(w) | vault(swap-ATA,w) | taker_give(w) | metadata(w) | edition
//   | owner_record(w) | dest_record(w) | token_metadata | spl_token | ata_prog | sysvar_ix | system
//   | rules_prog | rules | [if royalty: creator_ata * n  (the metadata acct above is reused)]
pub(crate) fn fill_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let expect_give = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let max_want = u64::from_le_bytes(d[40..48].try_into().unwrap());

    if accounts.len() < 23 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let [taker, swap, maker, give_mint, want_mint, taker_want, maker_want, want_token_prog, config, fee_ata, vault, taker_give, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, sysvar_ix, system, rules_prog, rules] =
        &accounts[..23]
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (state_maker, give_amount, want_amount, bump, royalty_mode, maker_pct) = load_fill_state(swap, give_mint.key, want_mint.key, taker.key)?;
    if give_amount != expect_give {
        return Err(ProgramError::InvalidArgument);
    }
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let swap_pda = Pubkey::create_program_address(&[b"swap", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if *vault.key != ata_for(&swap_pda, give_mint.key) || *taker_give.key != ata_for(taker.key, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
    }
    require_token_account(maker_want, want_mint.key, &state_maker)?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(want_token_prog, want_mint)?;

    // leg 1: taker pays the want-leg (fee + remainder to maker) + the negotiated royalty, sourced from the pNFT's
    // on-chain metadata (reused from leg 2). Royalty fills append the creator want-ATAs after the 23 fixed accounts.
    pay_want_and_royalty(
        program_id, config, want_token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, max_want,
        want_dec, nonce[0], royalty_mode, maker_pct, give_mint, Some(metadata), &accounts[23..],
    )?;

    // leg 2: release the pNFT from the vault to the taker (swap PDA signs; taker pays the dest rent)
    release_pnft(
        token_metadata, vault, swap, taker_give, taker, give_mint, metadata, edition, owner_record,
        dest_record, taker, system, sysvar_ix, spl_tok, ata_prog, rules_prog, rules, &state_maker, &nonce, bump,
    )?;
    // close the swap state (rent -> maker); the now-empty frozen vault ATA is left for a later rent-sweep
    close(swap, maker)
}

// ---- cancel_pnft (tag 20) -- maker reclaims the escrowed pNFT before any fill ----
// data: nonce[32]
// accounts: maker(s) | swap(pda,w) | give_mint | maker_give(w) | vault(swap-ATA,w) | config | fee_wallet(w)
//   | system | metadata(w) | edition | owner_record(w) | dest_record(w) | token_metadata | spl_token
//   | ata_prog | sysvar_ix | rules_prog | rules
pub(crate) fn cancel_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [maker, swap, give_mint, maker_give, vault, config, fee_wallet, system, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, sysvar_ix, rules_prog, rules] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
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
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        st[S_BUMP]
    };
    let swap_pda = Pubkey::create_program_address(&[b"swap", maker.key.as_ref(), &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if *vault.key != ata_for(&swap_pda, give_mint.key) || *maker_give.key != ata_for(maker.key, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
    }
    // delist fee: flat lamport charge to the configured fee wallet (anti-churn); the NFT still refunds to the maker
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
    // release the pNFT back to the maker (swap PDA signs; maker pays the dest rent)
    release_pnft(
        token_metadata, vault, swap, maker_give, maker, give_mint, metadata, edition, owner_record,
        dest_record, maker, system, sysvar_ix, spl_tok, ata_prog, rules_prog, rules, maker.key.as_ref(), &nonce, bump,
    )?;
    close(swap, maker)
}

// ---- expire_pnft (tag 21) -- permissionless refund of the pNFT to the maker after the deadline ----
// data: nonce[32]
// accounts: caller(s) | swap(pda,w) | maker(w) | give_mint | maker_give(w) | vault(swap-ATA,w) | metadata(w)
//   | edition | owner_record(w) | dest_record(w) | token_metadata | spl_token | ata_prog | sysvar_ix
//   | system | rules_prog | rules
pub(crate) fn expire_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [caller, swap, maker, give_mint, maker_give, vault, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, sysvar_ix, system, rules_prog, rules] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (state_maker, bump) = {
        let st = swap.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        let expiry = i64::from_le_bytes(st[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < expiry {
            return Err(ProgramError::InvalidArgument); // not yet expired
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
    if *vault.key != ata_for(&swap_pda, give_mint.key) || *maker_give.key != ata_for(maker.key, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
    }
    // refund the pNFT to the maker (swap PDA signs; the permissionless caller pays the dest rent)
    release_pnft(
        token_metadata, vault, swap, maker_give, maker, give_mint, metadata, edition, owner_record,
        dest_record, caller, system, sysvar_ix, spl_tok, ata_prog, rules_prog, rules, &state_maker, &nonce, bump,
    )?;
    close(swap, maker)
}
