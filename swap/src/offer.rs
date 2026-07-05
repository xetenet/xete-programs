//! Escrowed price bids against an open swap (tags 4-7).

use crate::cpi::*;
use crate::config::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

// ---- 7a. make_offer (tag 4) — escrow a price bid against an open swap ----
// data: offer_nonce[32] | want_amount u64 | expiry i64  (48)
// accounts: offerer(signer) | offer(pda) | ovault(pda) | want_mint | swap
//           | offerer_want_ata | token_program | system | config | alias
pub(crate) fn make_offer(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 48 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let ononce = arr32(&d[0..32])?;
    let want_amount = u64::from_le_bytes(d[32..40].try_into().unwrap());
    let expiry = i64::from_le_bytes(d[40..48].try_into().unwrap());

    let [offerer, offer, ovault, want_mint, swap, offerer_want, token_prog, system, config, alias] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !offerer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    if want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    // listing gate: the bidder must hold a registered alias (no-op if the gate is off)
    require_alias(program_id, config, offerer.key.as_ref(), alias)?;
    // capture the goods this bid is FOR, straight from the live listing, so the maker can't later
    // close+reopen the slot with worse terms and still accept
    let (give_mint_bytes, give_amount) = {
        let s = swap.try_borrow_data()?;
        if s.len() != SWAP_LEN || s[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &s[S_WANT_MINT..S_WANT_MINT + 32] != want_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        (
            arr32(&s[S_GIVE_MINT..S_GIVE_MINT + 32])?,
            u64::from_le_bytes(s[S_GIVE_AMT..S_GIVE_AMT + 8].try_into().unwrap()),
        )
    };

    let (offer_pda, obump) = Pubkey::find_program_address(&[b"offer", offerer.key.as_ref(), &ononce], program_id);
    if &offer_pda != offer.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (ovault_pda, ovbump) = Pubkey::find_program_address(&[b"ovault", offerer.key.as_ref(), &ononce], program_id);
    if &ovault_pda != ovault.key {
        return Err(ProgramError::InvalidSeeds);
    }

    let obs = [obump];
    create_pda(offerer, offer, system, OFFER_LEN, program_id, &[b"offer", offerer.key.as_ref(), &ononce, &obs])?;
    let ovbs = [ovbump];
    create_pda(offerer, ovault, system, TOKEN_ACCT_LEN, &spl_token::ID, &[b"ovault", offerer.key.as_ref(), &ononce, &ovbs])?;
    token_init(token_prog, ovault, want_mint, offer.key)?;
    token_transfer(token_prog, offerer_want, want_mint, ovault, offerer, want_amount, mint_decimals(want_mint)?, None)?;

    // write offer state
    let mut o = offer.try_borrow_mut_data()?;
    o[O_OFFERER..O_OFFERER + 32].copy_from_slice(offerer.key.as_ref());
    o[O_SWAP..O_SWAP + 32].copy_from_slice(swap.key.as_ref());
    o[O_WANT_MINT..O_WANT_MINT + 32].copy_from_slice(want_mint.key.as_ref());
    o[O_WANT_AMT..O_WANT_AMT + 8].copy_from_slice(&want_amount.to_le_bytes());
    o[O_GIVE_MINT..O_GIVE_MINT + 32].copy_from_slice(&give_mint_bytes);
    o[O_GIVE_AMT..O_GIVE_AMT + 8].copy_from_slice(&give_amount.to_le_bytes());
    o[O_EXPIRY..O_EXPIRY + 8].copy_from_slice(&expiry.to_le_bytes());
    o[O_STATUS] = STATUS_OPEN;
    o[O_BUMP] = obump;
    o[O_NONCE..O_NONCE + 32].copy_from_slice(&ononce);
    Ok(())
}

// ---- 7b. accept_offer (tag 5) — maker settles their swap against a chosen bid ----
// data: swap_nonce[32] | offer_nonce[32]  (64)
// accounts: maker(signer) | swap | swap_vault | offer | offer_vault | offerer
//           | give_mint | want_mint | offerer_give_ata | maker_want_ata | token_program | config | fee_ata
pub(crate) fn accept_offer(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 64 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let snonce = arr32(&d[0..32])?;
    let ononce = arr32(&d[32..64])?;

    let [maker, swap, swap_vault, offer, offer_vault, offerer, give_mint, want_mint, offerer_give, maker_want, token_prog, config, fee_ata] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id || offer.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let mka = *maker.key;
    let ofa = *offerer.key;

    // load + validate the swap (the listing)
    let (give_amount, swap_bump) = {
        let s = swap.try_borrow_data()?;
        if s.len() != SWAP_LEN || s[S_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &s[S_MAKER..S_MAKER + 32] != mka.as_ref() {
            return Err(ProgramError::IllegalOwner); // only the maker may accept
        }
        if &s[S_GIVE_MINT..S_GIVE_MINT + 32] != give_mint.key.as_ref()
            || &s[S_WANT_MINT..S_WANT_MINT + 32] != want_mint.key.as_ref()
        {
            return Err(ProgramError::InvalidArgument);
        }
        let expiry = i64::from_le_bytes(s[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp >= expiry {
            return Err(ProgramError::InvalidArgument);
        }
        (u64::from_le_bytes(s[S_GIVE_AMT..S_GIVE_AMT + 8].try_into().unwrap()), s[S_BUMP])
    };

    // load + validate the offer (the bid) — must target THIS swap
    let (want_amount, offer_bump, offer_give_mint, offer_give_amount) = {
        let o = offer.try_borrow_data()?;
        if o.len() != OFFER_LEN || o[O_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &o[O_SWAP..O_SWAP + 32] != swap.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        if &o[O_OFFERER..O_OFFERER + 32] != ofa.as_ref() || &o[O_WANT_MINT..O_WANT_MINT + 32] != want_mint.key.as_ref()
        {
            return Err(ProgramError::InvalidArgument);
        }
        let oexp = i64::from_le_bytes(o[O_EXPIRY..O_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp >= oexp {
            return Err(ProgramError::InvalidArgument);
        }
        (
            u64::from_le_bytes(o[O_WANT_AMT..O_WANT_AMT + 8].try_into().unwrap()),
            o[O_BUMP],
            arr32(&o[O_GIVE_MINT..O_GIVE_MINT + 32])?,
            u64::from_le_bytes(o[O_GIVE_AMT..O_GIVE_AMT + 8].try_into().unwrap()),
        )
    };

    // RUG GUARD: the listing's goods right now must equal what the bid agreed to buy.
    if offer_give_mint.as_ref() != give_mint.key.as_ref() || offer_give_amount != give_amount {
        return Err(ProgramError::InvalidArgument);
    }

    // verify all four PDAs
    let (swap_pda, _) = Pubkey::find_program_address(&[b"swap", mka.as_ref(), &snonce], program_id);
    let (svault_pda, _) = Pubkey::find_program_address(&[b"vault", mka.as_ref(), &snonce], program_id);
    let (offer_pda, _) = Pubkey::find_program_address(&[b"offer", ofa.as_ref(), &ononce], program_id);
    let (ovault_pda, _) = Pubkey::find_program_address(&[b"ovault", ofa.as_ref(), &ononce], program_id);
    if &swap_pda != swap.key || &svault_pda != swap_vault.key || &offer_pda != offer.key || &ovault_pda != offer_vault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    // recipients must belong to the right parties (no redirection)
    require_token_account(maker_want, want_mint.key, mka.as_ref())?;
    require_token_account(offerer_give, give_mint.key, ofa.as_ref())?;

    let give_dec = mint_decimals(give_mint)?;
    let want_dec = mint_decimals(want_mint)?;

    let obs = [offer_bump];
    let offer_seeds: &[&[u8]] = &[b"offer", ofa.as_ref(), &ononce, &obs];
    let sbs = [swap_bump];
    let swap_seeds: &[&[u8]] = &[b"swap", mka.as_ref(), &snonce, &sbs];

    // leg 1: the bid (escrowed want) -> maker, minus the protocol fee (offer PDA signs both)
    let (fee_bps, _delist, fee_wallet, fee_shards) = read_config(program_id, config)?;
    let fee = ((want_amount as u128 * fee_bps as u128) / BPS_DENOM as u128) as u64;
    if fee > 0 {
        verify_fee_dest(program_id, fee_ata, want_mint, &fee_wallet, fee_shards, snonce[0])?;
        token_transfer(token_prog, offer_vault, want_mint, fee_ata, offer, fee, want_dec, Some(offer_seeds))?;
    }
    token_transfer(token_prog, offer_vault, want_mint, maker_want, offer, want_amount - fee, want_dec, Some(offer_seeds))?;
    // leg 2: the goods (escrowed give) -> offerer  (swap PDA signs)
    token_transfer(token_prog, swap_vault, give_mint, offerer_give, swap, give_amount, give_dec, Some(swap_seeds))?;

    // close both vaults (rent -> respective party) then both state accounts
    token_close(token_prog, offer_vault, offerer, offer, offer_seeds)?;
    token_close(token_prog, swap_vault, maker, swap, swap_seeds)?;
    close(offer, offerer)?;
    close(swap, maker)
}

// ---- 7c. withdraw_offer (tag 6) — offerer reclaims their escrowed bid ----
// data: offer_nonce[32]
// accounts: offerer(signer) | offer | ovault | want_mint | offerer_want_ata | token_program
pub(crate) fn withdraw_offer(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let ononce = arr32(&d[0..32])?;

    let [offerer, offer, ovault, want_mint, offerer_want, token_prog] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !offerer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if offer.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let ofa = *offerer.key;
    let (want_amount, obump) = {
        let o = offer.try_borrow_data()?;
        if o.len() != OFFER_LEN || o[O_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &o[O_OFFERER..O_OFFERER + 32] != ofa.as_ref() {
            return Err(ProgramError::IllegalOwner); // only the offerer may withdraw
        }
        if &o[O_WANT_MINT..O_WANT_MINT + 32] != want_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        (u64::from_le_bytes(o[O_WANT_AMT..O_WANT_AMT + 8].try_into().unwrap()), o[O_BUMP])
    };
    let (offer_pda, _) = Pubkey::find_program_address(&[b"offer", ofa.as_ref(), &ononce], program_id);
    let (ovault_pda, _) = Pubkey::find_program_address(&[b"ovault", ofa.as_ref(), &ononce], program_id);
    if &offer_pda != offer.key || &ovault_pda != ovault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    require_token_account(offerer_want, want_mint.key, ofa.as_ref())?;
    let obs = [obump];
    let offer_seeds: &[&[u8]] = &[b"offer", ofa.as_ref(), &ononce, &obs];
    token_transfer(token_prog, ovault, want_mint, offerer_want, offer, want_amount, mint_decimals(want_mint)?, Some(offer_seeds))?;
    token_close(token_prog, ovault, offerer, offer, offer_seeds)?;
    close(offer, offerer)
}

// ---- 7d. expire_offer (tag 7) — permissionless refund of an expired bid to the offerer ----
// data: offer_nonce[32]
// accounts: caller(signer/payer) | offer | ovault | offerer | want_mint | offerer_want_ata | token_program
pub(crate) fn expire_offer(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let ononce = arr32(&d[0..32])?;

    let [_caller, offer, ovault, offerer, want_mint, offerer_want, token_prog] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if offer.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let ofa = *offerer.key;
    let (want_amount, obump) = {
        let o = offer.try_borrow_data()?;
        if o.len() != OFFER_LEN || o[O_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if &o[O_OFFERER..O_OFFERER + 32] != ofa.as_ref() {
            return Err(ProgramError::InvalidArgument); // offerer must be the bid's owner (rent + refund target)
        }
        if &o[O_WANT_MINT..O_WANT_MINT + 32] != want_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        let oexp = i64::from_le_bytes(o[O_EXPIRY..O_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < oexp {
            return Err(ProgramError::InvalidArgument); // not expired yet
        }
        (u64::from_le_bytes(o[O_WANT_AMT..O_WANT_AMT + 8].try_into().unwrap()), o[O_BUMP])
    };
    let (offer_pda, _) = Pubkey::find_program_address(&[b"offer", ofa.as_ref(), &ononce], program_id);
    let (ovault_pda, _) = Pubkey::find_program_address(&[b"ovault", ofa.as_ref(), &ononce], program_id);
    if &offer_pda != offer.key || &ovault_pda != ovault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    require_token_account(offerer_want, want_mint.key, ofa.as_ref())?;
    let obs = [obump];
    let offer_seeds: &[&[u8]] = &[b"offer", ofa.as_ref(), &ononce, &obs];
    token_transfer(token_prog, ovault, want_mint, offerer_want, offer, want_amount, mint_decimals(want_mint)?, Some(offer_seeds))?;
    token_close(token_prog, ovault, offerer, offer, offer_seeds)?;
    close(offer, offerer)
}
