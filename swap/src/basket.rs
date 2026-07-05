//! Bounded multi-mint baskets + bearer claim-by-key settlement (tags 12-17).

use crate::cpi::*;
use crate::config::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

// ---- 12. open_basket (tag 12) — escrow a give BASKET (≤5 mints), additive over the 1:1 open_swap ----
// data: nonce[32] | give_count u8 | want_count u8 | give_amts (give_count*u64)
//       | want entries (want_count * {mint[32]+amt u64[8]}) | expiry i64 | taker[32] | auth_mode u8 | claim_auth[32]
// accounts: maker(signer) | swap(pda) | system | token_program | config | alias
//           | THEN per give entry: give_mint | give_vault(pda) | maker_give_ata
// Give mints come from the accounts (needed for transfer_checked); give amounts from data.
// auth_mode=1 (bearer): want_count must be 0 and claim_auth must be set; claimed later by key, no taker fill.
pub(crate) fn open_basket(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() < 34 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let give_count = d[32] as usize;
    let want_count = d[33] as usize;
    if give_count < 1 || give_count > MAX_LEG || want_count > MAX_LEG {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amts_off = 34;
    let want_off = give_amts_off + give_count * 8;
    let tail_off = want_off + want_count * ENTRY;
    // tail = expiry(8) + taker(32) + auth_mode(1) + claim_auth(32)
    if d.len() != tail_off + 73 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let expiry = i64::from_le_bytes(d[tail_off..tail_off + 8].try_into().unwrap());
    let target_taker = arr32(&d[tail_off + 8..tail_off + 40])?;
    let auth_mode = d[tail_off + 40];
    let claim_auth = arr32(&d[tail_off + 41..tail_off + 73])?;

    if auth_mode != AUTH_NORMAL && auth_mode != AUTH_BEARER {
        return Err(ProgramError::InvalidInstructionData);
    }
    // Bearer is claimed by key (no taker fill): it must carry no fixed want and a real claim-auth hash.
    if auth_mode == AUTH_BEARER && (want_count != 0 || claim_auth == [0u8; 32]) {
        return Err(ProgramError::InvalidArgument);
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }

    if accounts.len() != 6 + give_count * 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let maker = &accounts[0];
    let swap = &accounts[1];
    let system = &accounts[2];
    let token_prog = &accounts[3];
    let config = &accounts[4];
    let alias = &accounts[5];
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    require_alias(program_id, config, maker.key.as_ref(), alias)?;

    let (swap_pda, bump) = Pubkey::find_program_address(&[b"basket", maker.key.as_ref(), &nonce], program_id);
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [bump];
    create_pda(maker, swap, system, BASKET_LEN, program_id, &[b"basket", maker.key.as_ref(), &nonce, &bs])?;

    // write the header + want basket (mints/amts straight from data)
    {
        let mut s = swap.try_borrow_mut_data()?;
        s[B_MAKER..B_MAKER + 32].copy_from_slice(maker.key.as_ref());
        s[B_GIVE_COUNT] = give_count as u8;
        s[B_WANT_COUNT] = want_count as u8;
        s[B_TAKER..B_TAKER + 32].copy_from_slice(&target_taker);
        s[B_AUTH_MODE] = auth_mode;
        s[B_CLAIM_AUTH..B_CLAIM_AUTH + 32].copy_from_slice(&claim_auth);
        s[B_EXPIRY..B_EXPIRY + 8].copy_from_slice(&expiry.to_le_bytes());
        s[B_STATUS] = STATUS_OPEN;
        s[B_BUMP] = bump;
        s[B_NONCE..B_NONCE + 32].copy_from_slice(&nonce);
        for j in 0..want_count {
            let e = want_off + j * ENTRY;
            let amt = u64::from_le_bytes(d[e + 32..e + 40].try_into().unwrap());
            if amt == 0 {
                return Err(ProgramError::InvalidInstructionData);
            }
            s[B_WANT + j * ENTRY..B_WANT + j * ENTRY + ENTRY].copy_from_slice(&d[e..e + ENTRY]);
        }
    }

    // per give entry: derive+create its vault, init to the swap-PDA authority, pull the leg in, record it
    for i in 0..give_count {
        let give_mint = &accounts[6 + i * 3];
        let give_vault = &accounts[6 + i * 3 + 1];
        let maker_give = &accounts[6 + i * 3 + 2];
        let amt = u64::from_le_bytes(d[give_amts_off + i * 8..give_amts_off + i * 8 + 8].try_into().unwrap());
        if amt == 0 {
            return Err(ProgramError::InvalidInstructionData);
        }
        // No selling an asset for itself: a give mint may not also appear in the want basket.
        {
            let s = swap.try_borrow_data()?;
            for j in 0..want_count {
                if s[B_WANT + j * ENTRY..B_WANT + j * ENTRY + 32] == *give_mint.key.as_ref() {
                    return Err(ProgramError::InvalidArgument);
                }
            }
        }
        let idx = [i as u8];
        let (vpda, vbump) = Pubkey::find_program_address(&[b"bvault", maker.key.as_ref(), &nonce, &idx], program_id);
        if &vpda != give_vault.key {
            return Err(ProgramError::InvalidSeeds);
        }
        let vbs = [vbump];
        create_pda(maker, give_vault, system, TOKEN_ACCT_LEN, &spl_token::ID, &[b"bvault", maker.key.as_ref(), &nonce, &idx, &vbs])?;
        token_init(token_prog, give_vault, give_mint, swap.key)?;
        token_transfer(token_prog, maker_give, give_mint, give_vault, maker, amt, mint_decimals(give_mint)?, None)?;
        let mut s = swap.try_borrow_mut_data()?;
        s[B_GIVE + i * ENTRY..B_GIVE + i * ENTRY + 32].copy_from_slice(give_mint.key.as_ref());
        s[B_GIVE + i * ENTRY + 32..B_GIVE + i * ENTRY + ENTRY].copy_from_slice(&amt.to_le_bytes());
    }
    Ok(())
}

// ---- 13. fill_basket (tag 13) — taker atomically settles a NORMAL basket swap ----
// data: nonce[32]
// accounts: taker(signer) | swap(pda) | maker | token_program | config
//           | per give entry: give_mint | give_vault(pda) | taker_give_ata     (give_count * 3)
//           | per want entry: want_mint | taker_want_ata | maker_want_ata | fee_ata  (want_count * 4)
// Big fills exceed the legacy account cap — the client builds a v0 tx (ALTs). Bearer/accept-a-bid
// offers are NOT filled here (bearer -> claim_bearer; want_count==0 -> accept_offer).
pub(crate) fn fill_basket(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let taker = &accounts[0];
    let swap = &accounts[1];
    let maker = &accounts[2];
    let token_prog = &accounts[3];
    let config = &accounts[4];
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    let (give_count, want_count, bump, state_maker) = {
        let s = swap.try_borrow_data()?;
        if s.len() != BASKET_LEN || s[B_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if s[B_AUTH_MODE] != AUTH_NORMAL {
            return Err(ProgramError::InvalidArgument); // bearer is claimed by key, not filled
        }
        let wc = s[B_WANT_COUNT] as usize;
        if wc == 0 {
            return Err(ProgramError::InvalidArgument); // accept-a-bid offer — settle via accept_offer
        }
        let expiry = i64::from_le_bytes(s[B_EXPIRY..B_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp >= expiry {
            return Err(ProgramError::InvalidArgument);
        }
        if s[B_TAKER..B_TAKER + 32].iter().any(|&b| b != 0) && &s[B_TAKER..B_TAKER + 32] != taker.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        (s[B_GIVE_COUNT] as usize, wc, s[B_BUMP], arr32(&s[B_MAKER..B_MAKER + 32])?)
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let swap_pda = Pubkey::create_program_address(&[b"basket", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if accounts.len() != 5 + give_count * 3 + want_count * 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let (fee_bps, _delist, fee_wallet, fee_shards) = read_config(program_id, config)?;
    let bs = [bump];
    let swap_seeds: &[&[u8]] = &[b"basket", state_maker.as_ref(), &nonce, &bs];

    // WANT legs: taker pays the maker for each requested currency; protocol fee skimmed per leg.
    let want_base = 5 + give_count * 3;
    for j in 0..want_count {
        let want_mint = &accounts[want_base + j * 4];
        let taker_want = &accounts[want_base + j * 4 + 1];
        let maker_want = &accounts[want_base + j * 4 + 2];
        let fee_ata = &accounts[want_base + j * 4 + 3];
        let (exp_mint, want_amount) = {
            let s = swap.try_borrow_data()?;
            (
                arr32(&s[B_WANT + j * ENTRY..B_WANT + j * ENTRY + 32])?,
                u64::from_le_bytes(s[B_WANT + j * ENTRY + 32..B_WANT + j * ENTRY + 40].try_into().unwrap()),
            )
        };
        if exp_mint.as_ref() != want_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        require_token_account(maker_want, want_mint.key, &state_maker)?;
        let want_dec = mint_decimals(want_mint)?;
        let fee = ((want_amount as u128 * fee_bps as u128) / BPS_DENOM as u128) as u64;
        if fee > 0 {
            verify_fee_dest(program_id, fee_ata, want_mint, &fee_wallet, fee_shards, nonce[0])?;
            token_transfer(token_prog, taker_want, want_mint, fee_ata, taker, fee, want_dec, None)?;
        }
        token_transfer(token_prog, taker_want, want_mint, maker_want, taker, want_amount - fee, want_dec, None)?;
    }

    // GIVE legs: each vault releases to the taker (swap PDA signs), then the empty vault is closed.
    for i in 0..give_count {
        let give_mint = &accounts[5 + i * 3];
        let give_vault = &accounts[5 + i * 3 + 1];
        let taker_give = &accounts[5 + i * 3 + 2];
        let (exp_mint, give_amount) = {
            let s = swap.try_borrow_data()?;
            (
                arr32(&s[B_GIVE + i * ENTRY..B_GIVE + i * ENTRY + 32])?,
                u64::from_le_bytes(s[B_GIVE + i * ENTRY + 32..B_GIVE + i * ENTRY + 40].try_into().unwrap()),
            )
        };
        if exp_mint.as_ref() != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        let idx = [i as u8];
        let (vpda, _) = Pubkey::find_program_address(&[b"bvault", state_maker.as_ref(), &nonce, &idx], program_id);
        if &vpda != give_vault.key {
            return Err(ProgramError::InvalidSeeds);
        }
        let give_dec = mint_decimals(give_mint)?;
        token_transfer(token_prog, give_vault, give_mint, taker_give, swap, give_amount, give_dec, Some(swap_seeds))?;
        token_close(token_prog, give_vault, maker, swap, swap_seeds)?;
    }

    close(swap, maker)
}

// ---- 14. claim_bearer (tag 14) — whoever holds the bearer key claims the give basket ----
// data: nonce[32]
// accounts: payer(signer, pays fees) | bearer(signer, the bearer key) | swap(pda) | maker(rent refund) | token_program
//           | per give entry: give_mint | give_vault(pda) | dest_ata   (give_count * 3)
// Auth: hash(bearer.key) must equal the stored B_CLAIM_AUTH (token<->item unlinkable on chain). The claim
// names the destination + is signed by the bearer key => front-run-safe. NO maker revoke path exists.
pub(crate) fn claim_bearer(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let payer = &accounts[0];
    let bearer = &accounts[1];
    let swap = &accounts[2];
    let maker = &accounts[3];
    let token_prog = &accounts[4];
    if !payer.is_signer || !bearer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    let (give_count, bump, state_maker) = {
        let s = swap.try_borrow_data()?;
        if s.len() != BASKET_LEN || s[B_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if s[B_AUTH_MODE] != AUTH_BEARER {
            return Err(ProgramError::InvalidArgument);
        }
        // The bearer key proves the claim; only its HASH is stored, so the token can't be tied to the item.
        let h = solana_program::hash::hash(bearer.key.as_ref());
        if &s[B_CLAIM_AUTH..B_CLAIM_AUTH + 32] != h.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        (s[B_GIVE_COUNT] as usize, s[B_BUMP], arr32(&s[B_MAKER..B_MAKER + 32])?)
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let swap_pda = Pubkey::create_program_address(&[b"basket", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if accounts.len() != 5 + give_count * 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let bs = [bump];
    let swap_seeds: &[&[u8]] = &[b"basket", state_maker.as_ref(), &nonce, &bs];
    for i in 0..give_count {
        let give_mint = &accounts[5 + i * 3];
        let give_vault = &accounts[5 + i * 3 + 1];
        let dest = &accounts[5 + i * 3 + 2]; // the claimer's chosen destination
        let (exp_mint, give_amount) = {
            let s = swap.try_borrow_data()?;
            (
                arr32(&s[B_GIVE + i * ENTRY..B_GIVE + i * ENTRY + 32])?,
                u64::from_le_bytes(s[B_GIVE + i * ENTRY + 32..B_GIVE + i * ENTRY + 40].try_into().unwrap()),
            )
        };
        if exp_mint.as_ref() != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        let idx = [i as u8];
        let (vpda, _) = Pubkey::find_program_address(&[b"bvault", state_maker.as_ref(), &nonce, &idx], program_id);
        if &vpda != give_vault.key {
            return Err(ProgramError::InvalidSeeds);
        }
        let dec = mint_decimals(give_mint)?;
        token_transfer(token_prog, give_vault, give_mint, dest, swap, give_amount, dec, Some(swap_seeds))?;
        token_close(token_prog, give_vault, maker, swap, swap_seeds)?;
    }
    close(swap, maker) // rent deposit returns to the maker; the claimer took the assets
}

// ---- 15. rewrap (tag 15) — current bearer re-keys the settlement to a new bearer hash ----
// data: nonce[32] | new_claim_auth[32]   accounts: bearer(signer) | swap(pda)
// Pass-it-along: the holder hands the next holder a fresh key; in-place re-key (no new account/rent).
pub(crate) fn rewrap(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 64 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    let new_auth = arr32(&d[32..64])?;
    if new_auth == [0u8; 32] {
        return Err(ProgramError::InvalidArgument);
    }
    let [bearer, swap] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !bearer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let state_maker = {
        let s = swap.try_borrow_data()?;
        if s.len() != BASKET_LEN || s[B_STATUS] != STATUS_OPEN || s[B_AUTH_MODE] != AUTH_BEARER {
            return Err(ProgramError::InvalidAccountData);
        }
        let h = solana_program::hash::hash(bearer.key.as_ref());
        if &s[B_CLAIM_AUTH..B_CLAIM_AUTH + 32] != h.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        arr32(&s[B_MAKER..B_MAKER + 32])?
    };
    let (swap_pda, _) = Pubkey::find_program_address(&[b"basket", &state_maker, &nonce], program_id);
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let mut s = swap.try_borrow_mut_data()?;
    s[B_CLAIM_AUTH..B_CLAIM_AUTH + 32].copy_from_slice(&new_auth);
    Ok(())
}

// ---- 16. cancel_basket (tag 16) — maker reclaims an unfilled NORMAL offer (bearer has NO revoke) ----
// data: nonce[32]
// accounts: maker(signer) | swap(pda) | token_program | config | fee_wallet | system
//           | per give entry: give_mint | give_vault(pda) | maker_give_ata
// NOTE: rent refunds to the maker here (matches the proven 1:1 path). "Rent kept on abandoned" is an
// economic policy that applies to expire-after-deadline and is layered separately (see audit notes).
pub(crate) fn cancel_basket(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    if accounts.len() < 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let maker = &accounts[0];
    let swap = &accounts[1];
    let token_prog = &accounts[2];
    let config = &accounts[3];
    let fee_wallet = &accounts[4];
    let system = &accounts[5];
    if !maker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (give_count, bump) = {
        let s = swap.try_borrow_data()?;
        if s.len() != BASKET_LEN || s[B_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if s[B_AUTH_MODE] == AUTH_BEARER {
            return Err(ProgramError::InvalidArgument); // bearer = no maker revoke
        }
        if &s[B_MAKER..B_MAKER + 32] != maker.key.as_ref() {
            return Err(ProgramError::IllegalOwner);
        }
        (s[B_GIVE_COUNT] as usize, s[B_BUMP])
    };
    let swap_pda = Pubkey::create_program_address(&[b"basket", maker.key.as_ref(), &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if accounts.len() != 6 + give_count * 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let (_bps, delist_fee, cfg_wallet, _shards) = read_config(program_id, config)?;
    if delist_fee > 0 {
        if fee_wallet.key.as_ref() != cfg_wallet.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        invoke(&system_instruction::transfer(maker.key, fee_wallet.key, delist_fee), &[maker.clone(), fee_wallet.clone(), system.clone()])?;
    }
    let bs = [bump];
    let swap_seeds: &[&[u8]] = &[b"basket", maker.key.as_ref(), &nonce, &bs];
    for i in 0..give_count {
        let give_mint = &accounts[6 + i * 3];
        let give_vault = &accounts[6 + i * 3 + 1];
        let maker_give = &accounts[6 + i * 3 + 2];
        let (exp_mint, amt) = {
            let s = swap.try_borrow_data()?;
            (
                arr32(&s[B_GIVE + i * ENTRY..B_GIVE + i * ENTRY + 32])?,
                u64::from_le_bytes(s[B_GIVE + i * ENTRY + 32..B_GIVE + i * ENTRY + 40].try_into().unwrap()),
            )
        };
        if exp_mint.as_ref() != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        require_token_account(maker_give, give_mint.key, maker.key.as_ref())?;
        let idx = [i as u8];
        let (vpda, _) = Pubkey::find_program_address(&[b"bvault", maker.key.as_ref(), &nonce, &idx], program_id);
        if &vpda != give_vault.key {
            return Err(ProgramError::InvalidSeeds);
        }
        let dec = mint_decimals(give_mint)?;
        token_transfer(token_prog, give_vault, give_mint, maker_give, swap, amt, dec, Some(swap_seeds))?;
        token_close(token_prog, give_vault, maker, swap, swap_seeds)?;
    }
    close(swap, maker)
}

// ---- 17. expire_basket (tag 17) — permissionless cleanup of an expired NORMAL offer; give->maker ----
// data: nonce[32]
// accounts: caller(signer) | swap(pda) | maker | token_program
//           | per give entry: give_mint | give_vault(pda) | maker_give_ata
// Bearer offers never auto-return (no revoke; lost key = stuck, by design). maker_give is verified to be
// the maker's ATA so a permissionless caller can't redirect the assets to themselves.
pub(crate) fn expire_basket(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 32 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let nonce = arr32(&d[0..32])?;
    if accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let _caller = &accounts[0];
    let swap = &accounts[1];
    let maker = &accounts[2];
    let token_prog = &accounts[3];
    if swap.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (give_count, bump, state_maker) = {
        let s = swap.try_borrow_data()?;
        if s.len() != BASKET_LEN || s[B_STATUS] != STATUS_OPEN {
            return Err(ProgramError::InvalidAccountData);
        }
        if s[B_AUTH_MODE] == AUTH_BEARER {
            return Err(ProgramError::InvalidArgument); // bearer never auto-returns (no revoke)
        }
        let expiry = i64::from_le_bytes(s[B_EXPIRY..B_EXPIRY + 8].try_into().unwrap());
        if Clock::get()?.unix_timestamp < expiry {
            return Err(ProgramError::InvalidArgument); // not expired yet
        }
        (s[B_GIVE_COUNT] as usize, s[B_BUMP], arr32(&s[B_MAKER..B_MAKER + 32])?)
    };
    if maker.key.as_ref() != state_maker.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    let swap_pda = Pubkey::create_program_address(&[b"basket", &state_maker, &nonce, &[bump]], program_id).map_err(|_| ProgramError::InvalidSeeds)?;
    if &swap_pda != swap.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if accounts.len() != 4 + give_count * 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let bs = [bump];
    let swap_seeds: &[&[u8]] = &[b"basket", state_maker.as_ref(), &nonce, &bs];
    for i in 0..give_count {
        let give_mint = &accounts[4 + i * 3];
        let give_vault = &accounts[4 + i * 3 + 1];
        let maker_give = &accounts[4 + i * 3 + 2];
        let (exp_mint, amt) = {
            let s = swap.try_borrow_data()?;
            (
                arr32(&s[B_GIVE + i * ENTRY..B_GIVE + i * ENTRY + 32])?,
                u64::from_le_bytes(s[B_GIVE + i * ENTRY + 32..B_GIVE + i * ENTRY + 40].try_into().unwrap()),
            )
        };
        if exp_mint.as_ref() != give_mint.key.as_ref() {
            return Err(ProgramError::InvalidArgument);
        }
        require_token_account(maker_give, give_mint.key, state_maker.as_ref())?;
        let idx = [i as u8];
        let (vpda, _) = Pubkey::find_program_address(&[b"bvault", state_maker.as_ref(), &nonce, &idx], program_id);
        if &vpda != give_vault.key {
            return Err(ProgramError::InvalidSeeds);
        }
        let dec = mint_decimals(give_mint)?;
        token_transfer(token_prog, give_vault, give_mint, maker_give, swap, amt, dec, Some(swap_seeds))?;
        token_close(token_prog, give_vault, maker, swap, swap_seeds)?;
    }
    close(swap, maker)
}
