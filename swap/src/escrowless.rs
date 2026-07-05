//! Escrowless listings: the asset stays in the maker's wallet via a standing delegate (tags 26-37).

use crate::cpi::*;
use crate::config::*;
use crate::settle::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

// ---- list (tag 26) -- ESCROWLESS: the NFT stays in the maker's wallet; the listing PDA is approved as delegate ----
// data: nonce[32] give_amount want_amount terms[32] expiry taker[32] royalty_mode u8 maker_pct u8  (122)
// accounts: maker(s) | listing(pda,w) | give_mint | want_mint | maker_give(w) | token_prog | system | config | alias
pub(crate) fn list(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 122 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[40..48].try_into().unwrap());
    let terms = arr32(&d[48..80])?;
    let expiry = i64::from_le_bytes(d[80..88].try_into().unwrap());
    let target_taker = arr32(&d[88..120])?;
    let royalty_mode = d[120];
    let maker_pct = d[121];
    if royalty_mode > ROYALTY_SPLIT || maker_pct > 100 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let [maker, listing, give_mint, want_mint, maker_give, token_prog, system, config, alias] = accounts else {
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
    assert_token_program(token_prog, give_mint)?;
    require_token_account(maker_give, give_mint.key, maker.key.as_ref())?; // the asset stays in the maker's own account
    require_alias(program_id, config, maker.key.as_ref(), alias)?;

    let (listing_pda, bump) = Pubkey::find_program_address(&[b"listing", maker.key.as_ref(), &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [bump];
    create_pda(maker, listing, system, SWAP_LEN, program_id, &[b"listing", maker.key.as_ref(), &nonce, &bs])?;
    crate::events::emit_post_created(crate::events::KIND_LISTING, maker.key, listing.key, give_mint.key, want_mint.key, give_amount, want_amount, expiry, &nonce);
    // delegate the give-leg to the listing PDA — no custody transfer; the NFT never moves here
    token_approve(token_prog, maker_give, listing, maker, give_amount)?;

    let mut st = listing.try_borrow_mut_data()?;
    st[S_MAKER..S_MAKER + 32].copy_from_slice(maker.key.as_ref());
    st[S_GIVE_MINT..S_GIVE_MINT + 32].copy_from_slice(give_mint.key.as_ref());
    st[S_GIVE_AMT..S_GIVE_AMT + 8].copy_from_slice(&give_amount.to_le_bytes());
    st[S_ROYALTY_MODE] = royalty_mode;
    st[S_MAKER_PCT] = maker_pct;
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

// ---- fill_listing (tag 27) -- atomic settle: taker pays want; the listing PDA (delegate) moves the NFT maker->taker ----
// data: nonce[32] expect_give u64 max_want u64  (48)
// accounts: taker(s) | listing(pda,w) | maker(w) | give_mint | want_mint | maker_give(w) | taker_give(w)
//   | taker_want(w) | maker_want(w) | token_prog | config | fee_ata(w) | [give_token_prog optional 13th]
pub(crate) fn fill_listing(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let expect_give = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let max_want = u64::from_le_bytes(d[40..48].try_into().unwrap());
    if accounts.len() < 12 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let [taker, listing, maker, give_mint, want_mint, maker_give, taker_give, taker_want, maker_want, token_prog, config, fee_ata] =
        &accounts[..12]
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let give_token_prog = accounts.get(12).unwrap_or(token_prog);
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (state_maker, give_amount, want_amount, bump, royalty_mode, maker_pct) = load_fill_state(listing, give_mint.key, want_mint.key, taker.key)?;
    if give_amount != expect_give {
        return Err(ProgramError::InvalidArgument); // give slippage; the want+royalty cap is enforced vs max_want below
    }
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let listing_pda = Pubkey::create_program_address(&[b"listing", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    // the NFT must come FROM the maker's own account and land in the taker's; the payment must land in the maker's
    require_token_account(maker_give, give_mint.key, &state_maker)?;
    require_token_account(maker_want, want_mint.key, &state_maker)?;
    let give_dec = mint_decimals(give_mint)?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(token_prog, want_mint)?;
    assert_token_program(give_token_prog, give_mint)?;

    // leg 1: taker pays the want-leg (fee + remainder to maker) + the negotiated royalty, destination sourced
    // from the asset's on-chain metadata. Royalty fills pass give_token_prog(12) + metadata(13) + creator ATAs(14..).
    let (metadata, creator_atas): (Option<&AccountInfo>, &[AccountInfo]) = if royalty_mode == ROYALTY_NONE {
        (None, &[])
    } else {
        if accounts.len() < 14 {
            return Err(ProgramError::NotEnoughAccountKeys);
        }
        (Some(&accounts[13]), &accounts[14..])
    };
    pay_want_and_royalty(
        program_id, config, token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, max_want,
        want_dec, nonce[0], royalty_mode, maker_pct, give_mint, metadata, creator_atas,
    )?;

    // leg 2: the listing PDA, as the approved delegate, moves the NFT straight maker -> taker (it never touched us)
    let bs = [bump];
    let seeds: &[&[u8]] = &[b"listing", state_maker.as_ref(), &nonce, &bs];
    token_transfer(give_token_prog, maker_give, give_mint, taker_give, listing, give_amount, give_dec, Some(seeds))?;
    // close the listing (rent -> maker). the delegate auto-clears once the full approved amount moved.
    close(listing, maker)
}

// ---- cancel_listing (tag 28) -- maker delists: revoke the delegate + close ----
// data: nonce[32]
// accounts: maker(s) | listing(pda,w) | give_mint | maker_give(w) | token_prog | config | fee_wallet(w) | system
pub(crate) fn cancel_listing(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [maker, listing, give_mint, maker_give, token_prog, config, fee_wallet, system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    {
        let st = listing.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &st[S_MAKER..S_MAKER + 32] != maker.key.as_ref() {
            return Err(ProgramError::IllegalOwner);
        }
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    require_token_account(maker_give, give_mint.key, maker.key.as_ref())?;
    let (listing_pda, _) = Pubkey::find_program_address(&[b"listing", maker.key.as_ref(), &nonce], program_id);
    if &listing_pda != listing.key {
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
    token_revoke(token_prog, maker_give, maker)?;
    close(listing, maker)
}

// ---- expire_listing (tag 29) -- permissionless cleanup after the deadline (closes the listing; maker re-owns fully) ----
// data: nonce[32]
// accounts: caller(s) | listing(pda,w) | maker(w)
// NOTE: a permissionless caller can't sign the maker's Revoke, so the (now-unusable) delegate is left for the maker to
// revoke at leisure — it is inert without an open listing, and the NFT never left the maker's wallet.
pub(crate) fn expire_listing(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [caller, listing, maker] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let state_maker = {
        let st = listing.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        let expiry = i64::from_le_bytes(st[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < expiry {
            return Err(ProgramError::InvalidArgument);
        }
        arr32(&st[S_MAKER..S_MAKER + 32])?
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let (listing_pda, _) = Pubkey::find_program_address(&[b"listing", &state_maker, &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    close(listing, maker)
}

// ---- list_pnft (tag 30) -- ESCROWLESS pNFT: delegate the pNFT (Token-Metadata) to the listing PDA; it never moves ----
// data: nonce[32] give_amount(=1) want_amount terms[32] expiry taker[32] royalty_mode u8 maker_pct u8  (122)
// accounts: maker(s) | listing(pda,w) | give_mint | want_mint | owner_ata(w) | token_record(w) | metadata(w)
//   | edition | token_metadata | spl_token | sysvar_ix | system | rules_prog | rules | config | alias
pub(crate) fn list_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 122 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[40..48].try_into().unwrap());
    let terms = arr32(&d[48..80])?;
    let expiry = i64::from_le_bytes(d[80..88].try_into().unwrap());
    let target_taker = arr32(&d[88..120])?;
    let royalty_mode = d[120];
    let maker_pct = d[121];
    if royalty_mode > ROYALTY_SPLIT || maker_pct > 100 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let [maker, listing, give_mint, want_mint, owner_ata, token_record, metadata, edition, token_metadata, spl_tok, sysvar_ix, system, rules_prog, rules, config, alias] =
        accounts
    else {
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
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if *owner_ata.key != ata_for(maker.key, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
    }
    require_alias(program_id, config, maker.key.as_ref(), alias)?;

    let (listing_pda, bump) = Pubkey::find_program_address(&[b"listing", maker.key.as_ref(), &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [bump];
    create_pda(maker, listing, system, SWAP_LEN, program_id, &[b"listing", maker.key.as_ref(), &nonce, &bs])?;
    crate::events::emit_post_created(crate::events::KIND_LISTING_PNFT, maker.key, listing.key, give_mint.key, want_mint.key, give_amount, want_amount, expiry, &nonce);
    // delegate the pNFT to the listing PDA (Token-Metadata records it in the token-record). No custody transfer.
    mpl_delegate_or_revoke(
        token_metadata, token_record, listing, metadata, edition, give_mint, owner_ata, maker, maker, system,
        sysvar_ix, spl_tok, rules_prog, rules, 1, false,
    )?;
    let mut st = listing.try_borrow_mut_data()?;
    st[S_MAKER..S_MAKER + 32].copy_from_slice(maker.key.as_ref());
    st[S_GIVE_MINT..S_GIVE_MINT + 32].copy_from_slice(give_mint.key.as_ref());
    st[S_GIVE_AMT..S_GIVE_AMT + 8].copy_from_slice(&1u64.to_le_bytes());
    st[S_ROYALTY_MODE] = royalty_mode;
    st[S_MAKER_PCT] = maker_pct;
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

// ---- fill_listing_pnft (tag 31) -- taker pays want; listing PDA (delegate) moves the pNFT straight maker->taker ----
// data: nonce[32] expect_give u64 max_want u64  (48)
// accounts: taker(s) | listing(pda,w) | maker(w) | give_mint | want_mint | taker_want(w) | maker_want(w)
//   | want_token_prog | config | fee_ata(w) | maker_ata(w) | taker_ata(w) | metadata(w) | edition | owner_record(w)
//   | dest_record(w) | token_metadata | spl_token | ata_prog | sysvar_ix | system | rules_prog | rules
//   | [if royalty: creator_ata * n  (the metadata acct above is reused for royalty)]
pub(crate) fn fill_listing_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let expect_give = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let max_want = u64::from_le_bytes(d[40..48].try_into().unwrap());

    if accounts.len() < 23 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let [taker, listing, maker, give_mint, want_mint, taker_want, maker_want, want_token_prog, config, fee_ata, maker_ata, taker_ata, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, sysvar_ix, system, rules_prog, rules] =
        &accounts[..23]
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (state_maker, give_amount, want_amount, bump, royalty_mode, maker_pct) = load_fill_state(listing, give_mint.key, want_mint.key, taker.key)?;
    if give_amount != expect_give {
        return Err(ProgramError::InvalidArgument);
    }
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let listing_pda = Pubkey::create_program_address(&[b"listing", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if *maker_ata.key != ata_for(maker.key, give_mint.key) || *taker_ata.key != ata_for(taker.key, give_mint.key) {
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

    // leg 2: the listing PDA, as the Token-Metadata transfer delegate, moves the pNFT straight maker -> taker
    let bs = [bump];
    let seeds: &[&[u8]] = &[b"listing", state_maker.as_ref(), &nonce, &bs];
    mpl_transfer(
        token_metadata, maker_ata, maker, taker_ata, taker, give_mint, metadata, edition, owner_record,
        dest_record, listing, taker, system, sysvar_ix, spl_tok, ata_prog, rules_prog, rules, 1, Some(seeds),
    )?;
    close(listing, maker)
}

// ---- cancel_listing_pnft (tag 32) -- maker delists: Token-Metadata Revoke + close ----
// data: nonce[32]
// accounts: maker(s) | listing(pda,w) | give_mint | owner_ata(w) | token_record(w) | metadata(w) | edition
//   | token_metadata | spl_token | sysvar_ix | system | rules_prog | rules | config | fee_wallet(w)
pub(crate) fn cancel_listing_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [maker, listing, give_mint, owner_ata, token_record, metadata, edition, token_metadata, spl_tok, sysvar_ix, system, rules_prog, rules, config, fee_wallet] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *sysvar_ix.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    {
        let st = listing.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &st[S_MAKER..S_MAKER + 32] != maker.key.as_ref() {
            return Err(ProgramError::IllegalOwner);
        }
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    let (listing_pda, _) = Pubkey::find_program_address(&[b"listing", maker.key.as_ref(), &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if *owner_ata.key != ata_for(maker.key, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
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
    mpl_delegate_or_revoke(
        token_metadata, token_record, listing, metadata, edition, give_mint, owner_ata, maker, maker, system,
        sysvar_ix, spl_tok, rules_prog, rules, 0, true,
    )?;
    close(listing, maker)
}

// ---- expire_listing_pnft (tag 33) -- permissionless cleanup after the deadline (closes the listing) ----
// data: nonce[32]
// accounts: caller(s) | listing(pda,w) | maker(w)
pub(crate) fn expire_listing_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [caller, listing, maker] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let state_maker = {
        let st = listing.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        let expiry = i64::from_le_bytes(st[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < expiry {
            return Err(ProgramError::InvalidArgument);
        }
        arr32(&st[S_MAKER..S_MAKER + 32])?
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let (listing_pda, _) = Pubkey::find_program_address(&[b"listing", &state_maker, &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    close(listing, maker)
}

// ---- list_core (tag 34) -- ESCROWLESS Core: add a TransferDelegate plugin + delegate it to the listing PDA ----
// data: nonce[32] give_amount(=1) want_amount terms[32] expiry taker[32]  (120)
// accounts: maker(s) | listing(pda,w) | asset(w) | want_mint | mpl_core | system | config | alias [| collection(w)]
//   COLLECTION-MEMBER Core assets append their collection (9th, WRITABLE per the captured blueprint) —
//   mpl-core rejects member plugin ops without it (MissingCollection/0x19, the Velvetfur class). The
//   8-account form is byte-identical to v1.1, so already-shipped clients keep working for standalone assets.
pub(crate) fn list_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 120 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[40..48].try_into().unwrap());
    let terms = arr32(&d[48..80])?;
    let expiry = i64::from_le_bytes(d[80..88].try_into().unwrap());
    let target_taker = arr32(&d[88..120])?;

    let (rest, coll_opt) = match accounts {
        [head @ .., coll] if head.len() == 8 => (head, Some(coll)),
        _ => (accounts, None),
    };
    let [maker, listing, asset, want_mint, mpl_core, system, config, alias] = rest else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    // Resolve the collection slot: the real account when appended (must be OWNED by mpl-core; mpl-core
    // itself verifies the asset actually belongs to it), else the None sentinel (= the core program id).
    let collection = match coll_opt {
        Some(c) if c.key != mpl_core.key => {
            if c.owner != mpl_core.key {
                return Err(ProgramError::IllegalOwner);
            }
            c
        }
        _ => mpl_core,
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

    let (listing_pda, bump) = Pubkey::find_program_address(&[b"listing", maker.key.as_ref(), &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [bump];
    create_pda(maker, listing, system, SWAP_LEN, program_id, &[b"listing", maker.key.as_ref(), &nonce, &bs])?;
    crate::events::emit_post_created(crate::events::KIND_LISTING_CORE, maker.key, listing.key, asset.key, want_mint.key, give_amount, want_amount, expiry, &nonce);
    // 1. add a TransferDelegate plugin (authority defaults to the owner/maker)
    mpl_core_plugin(mpl_core, asset, collection, maker, system, vec![0x02u8, 0x03u8, 0x00u8], None)?;
    // 2. delegate that plugin's authority to the listing PDA: ApprovePluginAuthorityV1(TransferDelegate, Address(listing))
    let mut approve = Vec::with_capacity(35);
    approve.push(0x08u8);
    approve.push(0x03u8); // PluginType::TransferDelegate
    approve.push(0x03u8); // Authority::Address
    approve.extend_from_slice(listing.key.as_ref());
    mpl_core_plugin(mpl_core, asset, collection, maker, system, approve, None)?;

    let mut st = listing.try_borrow_mut_data()?;
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

// ---- fill_listing_core (tag 35) -- taker pays want; the listing PDA (transfer delegate) moves the asset to the taker ----
// data: nonce[32] expect_give u64 max_want u64  (48)
// accounts: taker(s) | listing(pda,w) | maker(w) | asset(w) | want_mint | taker_want(w) | maker_want(w)
//   | want_token_prog | config | fee_ata(w) | mpl_core [| collection]
//   COLLECTION-MEMBER assets append their collection (12th, readonly in TransferV1 per the blueprint).
pub(crate) fn fill_listing_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let expect_give = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let max_want = u64::from_le_bytes(d[40..48].try_into().unwrap());

    let (rest, coll_opt) = match accounts {
        [head @ .., coll] if head.len() == 11 => (head, Some(coll)),
        _ => (accounts, None),
    };
    let [taker, listing, maker, asset, want_mint, taker_want, maker_want, want_token_prog, config, fee_ata, mpl_core] =
        rest
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let collection = match coll_opt {
        Some(c) if c.key != mpl_core.key => {
            if c.owner != mpl_core.key {
                return Err(ProgramError::IllegalOwner);
            }
            c
        }
        _ => mpl_core,
    };
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let (state_maker, give_amount, want_amount, bump, _royalty_mode, _maker_pct) = load_fill_state(listing, asset.key, want_mint.key, taker.key)?;
    if give_amount != expect_give || want_amount > max_want {
        return Err(ProgramError::InvalidArgument);
    }
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let listing_pda = Pubkey::create_program_address(&[b"listing", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    require_token_account(maker_want, want_mint.key, &state_maker)?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(want_token_prog, want_mint)?;

    // leg 1: want payment (fee skim + remainder to maker)
    pay_want_leg(program_id, config, want_token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, want_dec, nonce[0])?;

    // leg 2: the listing PDA, as the asset's TransferDelegate, transfers it straight maker -> taker
    let bs = [bump];
    let seeds: &[&[u8]] = &[b"listing", state_maker.as_ref(), &nonce, &bs];
    mpl_core_transfer(mpl_core, asset, collection, taker, listing, taker, true, Some(seeds))?;
    close(listing, maker)
}

// ---- cancel_listing_core (tag 36) -- maker delists: the listing PDA removes its TransferDelegate plugin + close ----
// data: nonce[32]
// accounts: maker(s) | listing(pda,w) | asset(w) | config | fee_wallet(w) | system | mpl_core [| collection(w)]
//   COLLECTION-MEMBER assets append their collection (8th, WRITABLE — plugin ops write it per the blueprint).
pub(crate) fn cancel_listing_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let (rest, coll_opt) = match accounts {
        [head @ .., coll] if head.len() == 7 => (head, Some(coll)),
        _ => (accounts, None),
    };
    let [maker, listing, asset, config, fee_wallet, system, mpl_core] = rest else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let collection = match coll_opt {
        Some(c) if c.key != mpl_core.key => {
            if c.owner != mpl_core.key {
                return Err(ProgramError::IllegalOwner);
            }
            c
        }
        _ => mpl_core,
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    {
        let st = listing.try_borrow_data()?;
        if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &st[S_MAKER..S_MAKER + 32] != maker.key.as_ref() {
            return Err(ProgramError::IllegalOwner);
        }
        if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != asset.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
    }
    let (listing_pda, _) = Pubkey::find_program_address(&[b"listing", maker.key.as_ref(), &nonce], program_id);
    if &listing_pda != listing.key {
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
    // undo the delegation as the OWNER (maker): Revoke (authority -> owner) then Remove -> asset back to pre-listing state
    mpl_core_plugin(mpl_core, asset, collection, maker, system, vec![0x0au8, 0x03u8], None)?; // RevokePluginAuthorityV1(TransferDelegate)
    mpl_core_plugin(mpl_core, asset, collection, maker, system, vec![0x04u8, 0x03u8], None)?; // RemovePlugin(TransferDelegate)
    close(listing, maker)
}

// ---- expire_listing_core (tag 37) -- permissionless: the listing PDA removes its plugin + close (cleanup) ----
// data: nonce[32]
// accounts: caller(s) | listing(pda,w) | maker(w) | asset(w)
pub(crate) fn expire_listing_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let [caller, listing, maker, asset] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if listing.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let state_maker = {
        let st = listing.try_borrow_data()?;
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
        arr32(&st[S_MAKER..S_MAKER + 32])?
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let (listing_pda, _) = Pubkey::find_program_address(&[b"listing", &state_maker, &nonce], program_id);
    if &listing_pda != listing.key {
        return Err(ProgramError::InvalidSeeds);
    }
    // permissionless cleanup: close the listing. The now-inert TransferDelegate plugin (authority = this dead PDA)
    // can move nothing without an open listing to sign, and the maker (owner) can revoke it at leisure.
    close(listing, maker)
}
