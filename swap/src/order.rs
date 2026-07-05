//! Sealed signed orders: settle a maker's off-chain ed25519-signed order, nothing rested on chain (tags 38-40).

use crate::cpi::*;
use crate::settle::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

pub(crate) const ED25519_PROGRAM: Pubkey = solana_program::pubkey!("Ed25519SigVerify111111111111111111111111111");
pub(crate) const ORDER_DOMAIN: &[u8] = b"AXTSWAP:order:1";
pub(crate) const BID_DOMAIN: &[u8] = b"AXTSWAP:bid:1";

/// Confirm the Ed25519 native program (in the SAME tx, immediately before us) verified `signer`'s signature over
/// exactly `message`. We never re-do the crypto — the native precompile already did — and we never trust any offset
/// field parsed out of its (attacker-influenceable) instruction data. Instead we RECONSTRUCT the exact canonical
/// single-signature layout that `solana_sdk::ed25519_instruction::new_ed25519_instruction` emits (and that our own
/// client mirrors) and compare it byte-for-byte, then read the pubkey + message at FIXED positions. This is the
/// audited-standard pattern: no attacker-supplied offset is ever dereferenced.
pub(crate) fn verify_ed25519_order(ix_sysvar: &AccountInfo, signer: &Pubkey, message: &[u8]) -> ProgramResult {
    use solana_program::sysvar::instructions::{load_current_index_checked, load_instruction_at_checked};
    if *ix_sysvar.key != solana_program::sysvar::instructions::ID {
        return Err(ProgramError::InvalidArgument);
    }
    let cur = load_current_index_checked(ix_sysvar)? as usize;
    if cur == 0 {
        return Err(ProgramError::InvalidArgument); // an ed25519-verify ix must sit immediately before us
    }
    let ed = load_instruction_at_checked(cur - 1, ix_sysvar)?;
    if ed.program_id != ED25519_PROGRAM {
        return Err(ProgramError::InvalidArgument);
    }
    ed25519_layout_ok(&ed.data, signer, message)
}

/// Pure, allocation-free check that `data` is EXACTLY the canonical single-signature Ed25519 native-program
/// instruction for `signer` over `message`. Split out (no AccountInfo / sysvar) so it can be exhaustively FUZZED
/// on the host. Panic-free by construction: the single exact-length gate makes every later index in-bounds.
///
/// Canonical layout (solana_sdk::ed25519_instruction):
///   [0]=1 (num sigs)  [1]=0 (pad)  [2..16]=Ed25519SignatureOffsets  [16..48]=pubkey  [48..112]=sig  [112..]=message
/// Ed25519SignatureOffsets = 7 LE u16: sig_off, sig_ix, pk_off, pk_ix, msg_off, msg_sz, msg_ix.
/// All three instruction indices MUST be u16::MAX — everything lives in THIS instruction's own data.
pub fn ed25519_layout_ok(data: &[u8], signer: &Pubkey, message: &[u8]) -> ProgramResult {
    const PREFIX: usize = 16;
    const PK_OFF: usize = 16;
    const SIG_OFF: usize = PK_OFF + 32; // 48
    const MSG_OFF: usize = SIG_OFF + 64; // 112
    let msg_sz = u16::try_from(message.len()).map_err(|_| ProgramError::InvalidArgument)?; // native size field is u16
    let total = MSG_OFF.checked_add(message.len()).ok_or(ProgramError::InvalidArgument)?;
    if data.len() != total {
        return Err(ProgramError::InvalidArgument); // exact length: rejects short data AND trailing slack
    }
    // Reconstruct the expected header + the one self-contained offsets struct, then compare byte-for-byte.
    let mut expect = [0u8; PREFIX];
    expect[0] = 1; // num signatures
    expect[1] = 0; // padding
    expect[2..4].copy_from_slice(&(SIG_OFF as u16).to_le_bytes());
    expect[4..6].copy_from_slice(&u16::MAX.to_le_bytes()); // signature instruction index
    expect[6..8].copy_from_slice(&(PK_OFF as u16).to_le_bytes());
    expect[8..10].copy_from_slice(&u16::MAX.to_le_bytes()); // pubkey instruction index
    expect[10..12].copy_from_slice(&(MSG_OFF as u16).to_le_bytes());
    expect[12..14].copy_from_slice(&msg_sz.to_le_bytes());
    expect[14..16].copy_from_slice(&u16::MAX.to_le_bytes()); // message instruction index
    if data[..PREFIX] != expect {
        return Err(ProgramError::InvalidArgument); // not the canonical self-contained layout
    }
    if &data[PK_OFF..PK_OFF + 32] != signer.as_ref() {
        return Err(ProgramError::InvalidArgument); // signed by someone other than the maker
    }
    if &data[MSG_OFF..MSG_OFF + message.len()] != message {
        return Err(ProgramError::InvalidArgument); // signed different terms than are being settled
    }
    Ok(())
}

// ---- settle_signed_order (tag 38) -- execute a maker's off-chain SIGNED order; nothing rested on-chain ----
// data: give_amount u64 | want_amount u64 | taker[32] | expiry i64 | nonce[32] | royalty_mode u8 | maker_pct u8  (90; royalty bytes ARE signed)
// accounts: taker(s) | maker(w) | give_mint | want_mint | maker_give(w) | taker_give(w) | taker_want(w)
//   | maker_want(w) | order_auth([b"order",maker]) | order_done([b"order_done",maker,nonce],w) | token_prog
//   | config | fee_ata(w) | system | instructions_sysvar | [give_token_prog(15) + metadata(16) + creator_ata*n(17..) if royalty]
pub(crate) fn settle_signed_order(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 90 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amount = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[8..16].try_into().unwrap());
    let order_taker = arr32(&d[16..48])?;
    let expiry = i64::from_le_bytes(d[48..56].try_into().unwrap());
    let nonce = arr32(&d[56..88])?;
    let royalty_mode = d[88];
    let maker_pct = d[89];
    if royalty_mode > ROYALTY_SPLIT || maker_pct > 100 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if accounts.len() < 15 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let [taker, maker, give_mint, want_mint, maker_give, taker_give, taker_want, maker_want, order_auth, order_done, token_prog, config, fee_ata, system, ix_sysvar] =
        &accounts[..15]
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let give_token_prog = accounts.get(15).unwrap_or(token_prog);
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if give_amount == 0 || want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument); // order lapsed
    }
    // private/targeted order: if a taker is named, only they may settle (the order was encrypted to them off-chain)
    if order_taker.iter().any(|&b| b != 0) && &order_taker != taker.key.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }

    // reconstruct the exact bytes the maker signed (INCL. the royalty terms) + prove they authorized them
    let mut msg = Vec::with_capacity(169);
    msg.extend_from_slice(ORDER_DOMAIN);
    msg.extend_from_slice(give_mint.key.as_ref());
    msg.extend_from_slice(&give_amount.to_le_bytes());
    msg.extend_from_slice(want_mint.key.as_ref());
    msg.extend_from_slice(&want_amount.to_le_bytes());
    msg.extend_from_slice(&order_taker);
    msg.extend_from_slice(&expiry.to_le_bytes());
    msg.extend_from_slice(&nonce);
    msg.push(royalty_mode);
    msg.push(maker_pct);
    verify_ed25519_order(ix_sysvar, maker.key, &msg)?;

    let (auth_pda, auth_bump) = Pubkey::find_program_address(&[b"order", maker.key.as_ref()], program_id);
    if &auth_pda != order_auth.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (done_pda, done_bump) = Pubkey::find_program_address(&[b"order_done", maker.key.as_ref(), &nonce], program_id);
    if &done_pda != order_done.key {
        return Err(ProgramError::InvalidSeeds);
    }
    // replay guard: create the per-order done-marker. create_pda reverts if it already exists → an order settles once.
    let dbs = [done_bump];
    create_pda(taker, order_done, system, 1, program_id, &[b"order_done", maker.key.as_ref(), &nonce, &dbs])?;

    // the give must leave the MAKER's own account and land in the taker's; the payment must land in the MAKER's
    require_token_account(maker_give, give_mint.key, maker.key.as_ref())?;
    require_token_account(maker_want, want_mint.key, maker.key.as_ref())?;
    let give_dec = mint_decimals(give_mint)?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(token_prog, want_mint)?;
    assert_token_program(give_token_prog, give_mint)?;

    // leg 1: taker pays the want-leg (fee + remainder to maker) + the negotiated royalty (sourced from the give
    // asset's on-chain metadata). The order is fixed-price + maker-signed, so no max_want cap (the taker accepts
    // their royalty share by settling). Royalty orders pass give_token_prog(15) + metadata(16) + creator ATAs(17..).
    let metadata: Option<&AccountInfo> = if royalty_mode == ROYALTY_NONE {
        None
    } else {
        if accounts.len() < 17 {
            return Err(ProgramError::NotEnoughAccountKeys);
        }
        Some(&accounts[16])
    };
    let creator_atas: &[AccountInfo] = if accounts.len() > 17 { &accounts[17..] } else { &[] };
    pay_want_and_royalty(
        program_id, config, token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, u64::MAX,
        want_dec, nonce[0], royalty_mode, maker_pct, give_mint, metadata, creator_atas,
    )?;

    // leg 2: the give-leg moves maker -> taker via the maker's standing delegate ([b"order",maker] signs)
    let abs = [auth_bump];
    let seeds: &[&[u8]] = &[b"order", maker.key.as_ref(), &abs];
    token_transfer(give_token_prog, maker_give, give_mint, taker_give, order_auth, give_amount, give_dec, Some(seeds))?;
    Ok(())
}

// ---- settle_signed_order_pnft (tag 39) -- settle a maker's off-chain SIGNED order for a programmable NFT ----
// data: give_amount(=1) u64 | want_amount u64 | taker[32] | expiry i64 | nonce[32]  (88)
// accounts: taker(s) | maker(w) | give_mint | want_mint | taker_want(w) | maker_want(w) | want_token_prog
//   | config | fee_ata(w) | order_auth([b"order",maker]) | order_done([b"order_done",maker,nonce],w)
//   | maker_ata(w) | taker_ata(w) | metadata(w) | edition | owner_record(w) | dest_record(w)
//   | token_metadata | spl_token | ata_prog | instructions_sysvar | system | rules_prog | rules
pub(crate) fn settle_signed_order_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 90 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amount = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[8..16].try_into().unwrap());
    let order_taker = arr32(&d[16..48])?;
    let expiry = i64::from_le_bytes(d[48..56].try_into().unwrap());
    let nonce = arr32(&d[56..88])?;
    let royalty_mode = d[88];
    let maker_pct = d[89];
    if royalty_mode > ROYALTY_SPLIT || maker_pct > 100 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if accounts.len() < 24 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let [taker, maker, give_mint, want_mint, taker_want, maker_want, want_token_prog, config, fee_ata, order_auth, order_done, maker_ata, taker_ata, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, ix_sysvar, system, rules_prog, rules] =
        &accounts[..24]
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if give_amount != 1 || want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData); // a pNFT give is always exactly 1
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    if order_taker.iter().any(|&b| b != 0) && &order_taker != taker.key.as_ref() {
        return Err(ProgramError::InvalidArgument); // targeted/private order: only the named taker may settle
    }
    if *token_metadata.key != TOKEN_METADATA_ID || *ix_sysvar.key != SYSVAR_INSTRUCTIONS_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    // bind the maker's off-chain signature to these exact terms (INCL. the royalty stance)
    let mut msg = Vec::with_capacity(169);
    msg.extend_from_slice(ORDER_DOMAIN);
    msg.extend_from_slice(give_mint.key.as_ref());
    msg.extend_from_slice(&give_amount.to_le_bytes());
    msg.extend_from_slice(want_mint.key.as_ref());
    msg.extend_from_slice(&want_amount.to_le_bytes());
    msg.extend_from_slice(&order_taker);
    msg.extend_from_slice(&expiry.to_le_bytes());
    msg.extend_from_slice(&nonce);
    msg.push(royalty_mode);
    msg.push(maker_pct);
    verify_ed25519_order(ix_sysvar, maker.key, &msg)?;

    let (auth_pda, auth_bump) = Pubkey::find_program_address(&[b"order", maker.key.as_ref()], program_id);
    if &auth_pda != order_auth.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (done_pda, done_bump) = Pubkey::find_program_address(&[b"order_done", maker.key.as_ref(), &nonce], program_id);
    if &done_pda != order_done.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let dbs = [done_bump];
    create_pda(taker, order_done, system, 1, program_id, &[b"order_done", maker.key.as_ref(), &nonce, &dbs])?;

    if *maker_ata.key != ata_for(maker.key, give_mint.key) || *taker_ata.key != ata_for(taker.key, give_mint.key) {
        return Err(ProgramError::InvalidArgument);
    }
    require_token_account(maker_want, want_mint.key, maker.key.as_ref())?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(want_token_prog, want_mint)?;

    // leg 1: taker pays the want-leg (fee + remainder to maker) + the negotiated royalty (sourced from the pNFT's
    // on-chain metadata, reused from leg 2). Fixed-price signed order => no max_want cap. Royalty orders append
    // the creator want-ATAs after the 24 fixed accounts.
    pay_want_and_royalty(
        program_id, config, want_token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, u64::MAX,
        want_dec, nonce[0], royalty_mode, maker_pct, give_mint, Some(metadata), &accounts[24..],
    )?;

    // leg 2: order_auth, the standing Token-Metadata transfer delegate, moves the pNFT straight maker -> taker
    let abs = [auth_bump];
    let seeds: &[&[u8]] = &[b"order", maker.key.as_ref(), &abs];
    mpl_transfer(
        token_metadata, maker_ata, maker, taker_ata, taker, give_mint, metadata, edition, owner_record,
        dest_record, order_auth, taker, system, ix_sysvar, spl_tok, ata_prog, rules_prog, rules, 1, Some(seeds),
    )?;
    Ok(())
}

// ---- settle_signed_order_core (tag 40) -- settle a maker's off-chain SIGNED order for a Metaplex Core asset ----
// data: give_amount(=1) u64 | want_amount u64 | taker[32] | expiry i64 | nonce[32]  (88)   [give_mint == asset]
// accounts: taker(s) | maker(w) | asset(w) | want_mint | taker_want(w) | maker_want(w) | want_token_prog
//   | config | fee_ata(w) | order_auth([b"order",maker]) | order_done([b"order_done",maker,nonce],w)
//   | system | instructions_sysvar | mpl_core [| collection]
//   COLLECTION-MEMBER assets append their collection (15th, readonly in TransferV1 per the blueprint).
pub(crate) fn settle_signed_order_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 90 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amount = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[8..16].try_into().unwrap());
    let order_taker = arr32(&d[16..48])?;
    let expiry = i64::from_le_bytes(d[48..56].try_into().unwrap());
    let nonce = arr32(&d[56..88])?;
    let royalty_mode = d[88];
    let maker_pct = d[89];
    if royalty_mode != ROYALTY_NONE {
        return Err(ProgramError::InvalidInstructionData); // Core royalty deferred (plugin parser not yet built)
    }
    let (rest, coll_opt) = match accounts {
        [head @ .., coll] if head.len() == 14 => (head, Some(coll)),
        _ => (accounts, None),
    };
    let [taker, maker, asset, want_mint, taker_want, maker_want, want_token_prog, config, fee_ata, order_auth, order_done, system, ix_sysvar, mpl_core] =
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
    if give_amount != 1 || want_amount == 0 {
        return Err(ProgramError::InvalidInstructionData); // a Core asset give is always exactly 1
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    if order_taker.iter().any(|&b| b != 0) && &order_taker != taker.key.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    // the asset's own key plays the give_mint role in the signed message
    let mut msg = Vec::with_capacity(169);
    msg.extend_from_slice(ORDER_DOMAIN);
    msg.extend_from_slice(asset.key.as_ref());
    msg.extend_from_slice(&give_amount.to_le_bytes());
    msg.extend_from_slice(want_mint.key.as_ref());
    msg.extend_from_slice(&want_amount.to_le_bytes());
    msg.extend_from_slice(&order_taker);
    msg.extend_from_slice(&expiry.to_le_bytes());
    msg.extend_from_slice(&nonce);
    msg.push(royalty_mode);
    msg.push(maker_pct);
    verify_ed25519_order(ix_sysvar, maker.key, &msg)?;

    let (auth_pda, auth_bump) = Pubkey::find_program_address(&[b"order", maker.key.as_ref()], program_id);
    if &auth_pda != order_auth.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (done_pda, done_bump) = Pubkey::find_program_address(&[b"order_done", maker.key.as_ref(), &nonce], program_id);
    if &done_pda != order_done.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let dbs = [done_bump];
    create_pda(taker, order_done, system, 1, program_id, &[b"order_done", maker.key.as_ref(), &nonce, &dbs])?;

    require_token_account(maker_want, want_mint.key, maker.key.as_ref())?;
    let want_dec = mint_decimals(want_mint)?;
    assert_token_program(want_token_prog, want_mint)?;

    // leg 1: taker pays the want-leg (fee skim + remainder to maker)
    pay_want_leg(program_id, config, want_token_prog, taker_want, want_mint, fee_ata, maker_want, taker, want_amount, want_dec, nonce[0])?;

    // leg 2: order_auth, the asset's TransferDelegate, transfers it straight maker -> taker
    let abs = [auth_bump];
    let seeds: &[&[u8]] = &[b"order", maker.key.as_ref(), &abs];
    mpl_core_transfer(mpl_core, asset, collection, taker, order_auth, taker, true, Some(seeds))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// SEALED SIGNED BIDS (tags 41 / 43): a BUYER signs "coins for THIS NFT" off-chain; the NFT's owner
// accepts + settles. The reverse of settle_signed_order — give = coins (the bidder, pulled via the
// bidder's standing delegate) and want = the NFT (the owner, transferred under the owner's OWN
// signature, no delegate). v1 carries NO royalty (royalty_mode must be NONE); pNFT-want (ruleset) is a
// fast-follow. BID_DOMAIN cryptographically separates a bid from a sell so neither can settle as the other.
// data (90): give_amount(coins) u64 | want_amount(=1) u64 | order_taker[32] | expiry i64 | nonce[32] | royalty_mode u8 | maker_pct u8
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Shared bid gates: signer (the OWNER), amounts, expiry, taker-binding, the bidder's ed25519 signature
/// over the exact bid bytes (bound to `want_key` = the NFT), the order PDA + replay marker. Returns the
/// order-auth bump. Mirrors the settle_signed_order preamble, reversed for the buy direction.
#[allow(clippy::too_many_arguments)]
fn verify_bid_common<'a>(
    program_id: &Pubkey, ix_sysvar: &AccountInfo<'a>, maker: &AccountInfo<'a>, taker: &AccountInfo<'a>, system: &AccountInfo<'a>,
    order_auth: &AccountInfo<'a>, order_done: &AccountInfo<'a>, coins_mint_key: &Pubkey, want_key: &Pubkey,
    give_amount: u64, want_amount: u64, order_taker: &[u8; 32], expiry: i64, nonce: &[u8; 32],
    royalty_mode: u8, maker_pct: u8,
) -> Result<u8, ProgramError> {
    if !taker.is_signer {
        return Err(ProgramError::MissingRequiredSignature); // the OWNER settles + authorizes the NFT move
    }
    if give_amount == 0 || want_amount != 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if royalty_mode != ROYALTY_NONE || maker_pct != 0 {
        return Err(ProgramError::InvalidInstructionData); // v1: bids do not carry royalty terms yet
    }
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    if order_taker.iter().any(|&b| b != 0) && order_taker != taker.key.as_ref() {
        return Err(ProgramError::InvalidArgument); // the bid is addressed to THIS owner only
    }
    let mut msg = Vec::with_capacity(169);
    msg.extend_from_slice(BID_DOMAIN);
    msg.extend_from_slice(coins_mint_key.as_ref());
    msg.extend_from_slice(&give_amount.to_le_bytes());
    msg.extend_from_slice(want_key.as_ref());
    msg.extend_from_slice(&want_amount.to_le_bytes());
    msg.extend_from_slice(order_taker);
    msg.extend_from_slice(&expiry.to_le_bytes());
    msg.extend_from_slice(nonce);
    msg.push(royalty_mode);
    msg.push(maker_pct);
    verify_ed25519_order(ix_sysvar, maker.key, &msg)?; // maker = the BIDDER authorized this exact bid

    let (auth_pda, auth_bump) = Pubkey::find_program_address(&[b"order", maker.key.as_ref()], program_id);
    if &auth_pda != order_auth.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let (done_pda, done_bump) = Pubkey::find_program_address(&[b"order_done", maker.key.as_ref(), nonce], program_id);
    if &done_pda != order_done.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let dbs = [done_bump];
    create_pda(taker, order_done, system, 1, program_id, &[b"order_done", maker.key.as_ref(), nonce, &dbs])?;
    Ok(auth_bump)
}

// ---- settle_signed_bid_core (tag 43) -- accept a buyer's signed bid for a Metaplex Core asset ----
// accounts: taker(owner,s) | maker(bidder,w) | coins_mint | asset(w) | maker_coins(w) | owner_coins(w)
//   | coins_token_prog | config | fee_ata(w) | order_auth | order_done(w) | system | ix_sysvar | mpl_core [| collection]
pub(crate) fn settle_signed_bid_core(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 90 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amount = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[8..16].try_into().unwrap());
    let order_taker = arr32(&d[16..48])?;
    let expiry = i64::from_le_bytes(d[48..56].try_into().unwrap());
    let nonce = arr32(&d[56..88])?;
    let royalty_mode = d[88];
    let maker_pct = d[89];
    let (rest, coll_opt) = match accounts {
        [head @ .., coll] if head.len() == 14 => (head, Some(coll)),
        _ => (accounts, None),
    };
    let [taker, maker, coins_mint, asset, maker_coins, owner_coins, coins_token_prog, config, fee_ata, order_auth, order_done, system, ix_sysvar, mpl_core] =
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
    if *mpl_core.key != MPL_CORE_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let auth_bump = verify_bid_common(program_id, ix_sysvar, maker, taker, system, order_auth, order_done,
        coins_mint.key, asset.key, give_amount, want_amount, &order_taker, expiry, &nonce, royalty_mode, maker_pct)?;

    require_token_account(maker_coins, coins_mint.key, maker.key.as_ref())?; // bidder's coins (source)
    require_token_account(owner_coins, coins_mint.key, taker.key.as_ref())?; // owner's proceeds
    let coins_dec = mint_decimals(coins_mint)?;
    assert_token_program(coins_token_prog, coins_mint)?;

    // leg 1: the bidder's coins -> protocol fee + the owner, via the bidder's standing delegate.
    let abs = [auth_bump];
    let seeds: &[&[u8]] = &[b"order", maker.key.as_ref(), &abs];
    pay_coins_via_delegate(program_id, config, coins_token_prog, maker_coins, coins_mint, fee_ata, owner_coins,
        order_auth, seeds, give_amount, coins_dec, nonce[0])?;

    // leg 2: the Core asset -> the bidder, transferred by the OWNER (taker signs; no delegate).
    mpl_core_transfer(mpl_core, asset, collection, taker, taker, maker, true, None)?;
    Ok(())
}

// ---- settle_signed_bid_spl (tag 41) -- accept a buyer's signed bid for a classic SPL 1-of-1 ----
// accounts: taker(owner,s) | maker(bidder,w) | coins_mint | nft_mint | maker_coins(w) | owner_coins(w)
//   | coins_token_prog | config | fee_ata(w) | order_auth | order_done(w) | system | ix_sysvar
//   | owner_nft(w) | maker_nft(w) | nft_token_prog
pub(crate) fn settle_signed_bid_spl(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 90 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amount = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[8..16].try_into().unwrap());
    let order_taker = arr32(&d[16..48])?;
    let expiry = i64::from_le_bytes(d[48..56].try_into().unwrap());
    let nonce = arr32(&d[56..88])?;
    let royalty_mode = d[88];
    let maker_pct = d[89];
    let [taker, maker, coins_mint, nft_mint, maker_coins, owner_coins, coins_token_prog, config, fee_ata, order_auth, order_done, system, ix_sysvar, owner_nft, maker_nft, nft_token_prog] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let auth_bump = verify_bid_common(program_id, ix_sysvar, maker, taker, system, order_auth, order_done,
        coins_mint.key, nft_mint.key, give_amount, want_amount, &order_taker, expiry, &nonce, royalty_mode, maker_pct)?;

    require_token_account(maker_coins, coins_mint.key, maker.key.as_ref())?;
    require_token_account(owner_coins, coins_mint.key, taker.key.as_ref())?;
    require_token_account(owner_nft, nft_mint.key, taker.key.as_ref())?; // owner holds the 1-of-1
    require_token_account(maker_nft, nft_mint.key, maker.key.as_ref())?; // bidder's destination
    let coins_dec = mint_decimals(coins_mint)?;
    assert_token_program(coins_token_prog, coins_mint)?;
    assert_token_program(nft_token_prog, nft_mint)?;

    // leg 1: the bidder's coins -> fee + owner, via the bidder's standing delegate.
    let abs = [auth_bump];
    let seeds: &[&[u8]] = &[b"order", maker.key.as_ref(), &abs];
    pay_coins_via_delegate(program_id, config, coins_token_prog, maker_coins, coins_mint, fee_ata, owner_coins,
        order_auth, seeds, give_amount, coins_dec, nonce[0])?;

    // leg 2: the NFT -> the bidder, transferred by the OWNER (taker signs; 0-decimal, amount 1).
    token_transfer(nft_token_prog, owner_nft, nft_mint, maker_nft, taker, 1, 0, None)?;
    Ok(())
}


// ---- settle_signed_bid_pnft (tag 42) -- accept a buyer's signed bid for a programmable NFT ----
// accounts: taker(owner,s) | maker(bidder,w) | coins_mint | nft_mint | maker_coins(w) | owner_coins(w)
//   | coins_token_prog | config | fee_ata(w) | order_auth | order_done(w) | system | ix_sysvar
//   | owner_nft(w) | maker_nft(w) | metadata(w) | edition | owner_record(w) | dest_record(w)
//   | token_metadata | spl_token | ata_prog | rules_prog | rules
// The pNFT moves owner -> bidder under the OWNER's own signature (no delegate); v1 royalty = NONE (the
// asset's ruleset still governs the transfer itself — a plain owner transfer passes a standard ruleset).
pub(crate) fn settle_signed_bid_pnft(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 90 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let give_amount = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let want_amount = u64::from_le_bytes(d[8..16].try_into().unwrap());
    let order_taker = arr32(&d[16..48])?;
    let expiry = i64::from_le_bytes(d[48..56].try_into().unwrap());
    let nonce = arr32(&d[56..88])?;
    let royalty_mode = d[88];
    let maker_pct = d[89];
    let [taker, maker, coins_mint, nft_mint, maker_coins, owner_coins, coins_token_prog, config, fee_ata, order_auth, order_done, system, ix_sysvar, owner_nft, maker_nft, metadata, edition, owner_record, dest_record, token_metadata, spl_tok, ata_prog, rules_prog, rules] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if *token_metadata.key != TOKEN_METADATA_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let auth_bump = verify_bid_common(program_id, ix_sysvar, maker, taker, system, order_auth, order_done,
        coins_mint.key, nft_mint.key, give_amount, want_amount, &order_taker, expiry, &nonce, royalty_mode, maker_pct)?;

    require_token_account(maker_coins, coins_mint.key, maker.key.as_ref())?;
    require_token_account(owner_coins, coins_mint.key, taker.key.as_ref())?;
    let coins_dec = mint_decimals(coins_mint)?;
    assert_token_program(coins_token_prog, coins_mint)?;

    // leg 1: the bidder's coins -> protocol fee + the owner, via the bidder's standing delegate.
    let abs = [auth_bump];
    let seeds: &[&[u8]] = &[b"order", maker.key.as_ref(), &abs];
    pay_coins_via_delegate(program_id, config, coins_token_prog, maker_coins, coins_mint, fee_ata, owner_coins,
        order_auth, seeds, give_amount, coins_dec, nonce[0])?;

    // leg 2: the pNFT -> the bidder, transferred by the OWNER (taker is token_owner + authority; no delegate).
    mpl_transfer(
        token_metadata, owner_nft, taker, maker_nft, maker, nft_mint, metadata, edition, owner_record,
        dest_record, taker, taker, system, ix_sysvar, spl_tok, ata_prog, rules_prog, rules, 1, None,
    )?;
    Ok(())
}


#[cfg(test)]
mod ed25519_layout_tests {
    use super::ed25519_layout_ok;
    use solana_program::pubkey::Pubkey;

    // Build a canonical single-signature Ed25519 native-program instruction data blob:
    //   [1,0] | offsets(14) | pubkey(32)@16 | sig(64)@48 | message@112
    fn canonical(signer: &Pubkey, sig: &[u8; 64], msg: &[u8]) -> Vec<u8> {
        let (pk_off, sig_off, msg_off) = (16u16, 48u16, 112u16);
        let mut d = vec![1u8, 0u8];
        d.extend_from_slice(&sig_off.to_le_bytes());
        d.extend_from_slice(&u16::MAX.to_le_bytes());
        d.extend_from_slice(&pk_off.to_le_bytes());
        d.extend_from_slice(&u16::MAX.to_le_bytes());
        d.extend_from_slice(&msg_off.to_le_bytes());
        d.extend_from_slice(&(msg.len() as u16).to_le_bytes());
        d.extend_from_slice(&u16::MAX.to_le_bytes());
        d.extend_from_slice(signer.as_ref());
        d.extend_from_slice(sig);
        d.extend_from_slice(msg);
        d
    }

    fn signer() -> Pubkey { Pubkey::new_from_array([7u8; 32]) }
    fn sig() -> [u8; 64] { [9u8; 64] }
    fn msg() -> Vec<u8> { b"AXTSWAP:order:1|terms-and-nonce".to_vec() }

    #[test]
    fn accepts_canonical() {
        let d = canonical(&signer(), &sig(), &msg());
        assert!(ed25519_layout_ok(&d, &signer(), &msg()).is_ok());
    }

    #[test]
    fn accepts_empty_message() {
        // Degenerate but well-formed: msg_sz = 0, total = 112. Must not panic; must accept if canonical.
        let d = canonical(&signer(), &sig(), &[]);
        assert!(ed25519_layout_ok(&d, &signer(), &[]).is_ok());
    }

    #[test]
    fn rejects_wrong_signer() {
        let d = canonical(&signer(), &sig(), &msg());
        assert!(ed25519_layout_ok(&d, &Pubkey::new_from_array([8u8; 32]), &msg()).is_err());
    }

    #[test]
    fn rejects_wrong_message_same_len() {
        let d = canonical(&signer(), &sig(), &msg());
        let mut m = msg(); let last = m.len() - 1; m[last] ^= 0x01;
        assert!(ed25519_layout_ok(&d, &signer(), &m).is_err());
    }

    #[test]
    fn rejects_message_the_signed_blob_did_not_contain() {
        // data signs msg(), but we ask to settle a DIFFERENT message => must reject (this is the whole point).
        let d = canonical(&signer(), &sig(), &msg());
        assert!(ed25519_layout_ok(&d, &signer(), b"totally different terms").is_err());
    }

    #[test]
    fn rejects_truncated() {
        let d = canonical(&signer(), &sig(), &msg());
        assert!(ed25519_layout_ok(&d[..d.len() - 1], &signer(), &msg()).is_err());
        assert!(ed25519_layout_ok(&d[..50], &signer(), &msg()).is_err());
        assert!(ed25519_layout_ok(&[], &signer(), &msg()).is_err());
    }

    #[test]
    fn rejects_trailing_slack() {
        let mut d = canonical(&signer(), &sig(), &msg());
        d.push(0x00); // one extra byte after the message
        assert!(ed25519_layout_ok(&d, &signer(), &msg()).is_err());
    }

    #[test]
    fn rejects_bad_header() {
        let mut d = canonical(&signer(), &sig(), &msg()); d[0] = 2; // num_sig != 1
        assert!(ed25519_layout_ok(&d, &signer(), &msg()).is_err());
        let mut d = canonical(&signer(), &sig(), &msg()); d[1] = 1; // padding != 0
        assert!(ed25519_layout_ok(&d, &signer(), &msg()).is_err());
    }

    #[test]
    fn rejects_non_selfcontained_indices() {
        // Any instruction index != u16::MAX means the sig/pk/msg could live in ANOTHER instruction — reject.
        for pos in [4usize, 8, 14] {
            let mut d = canonical(&signer(), &sig(), &msg());
            d[pos] = 0x00; d[pos + 1] = 0x00; // set that index to 0 instead of 0xFFFF
            assert!(ed25519_layout_ok(&d, &signer(), &msg()).is_err(), "index at {pos} not enforced");
        }
    }

    #[test]
    fn rejects_shifted_offsets() {
        // Tamper each offset field; canonical values are the ONLY accepted ones.
        for pos in [2usize, 6, 10, 12] {
            let mut d = canonical(&signer(), &sig(), &msg());
            d[pos] = d[pos].wrapping_add(1);
            assert!(ed25519_layout_ok(&d, &signer(), &msg()).is_err(), "offset at {pos} not pinned");
        }
    }
}

#[cfg(test)]
mod ed25519_fuzz_smoke {
    use super::ed25519_layout_ok;
    use solana_program::pubkey::Pubkey;
    // Deterministic xorshift PRNG (no dev-deps): 500k arbitrary (ix_data, message) pairs must NEVER panic.
    #[test]
    fn never_panics_on_arbitrary_input() {
        let mut s: u64 = 0x9E3779B97F4A7C15;
        let mut rng = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; s };
        let signer = Pubkey::new_from_array([7u8; 32]);
        for _ in 0..500_000u32 {
            let n = (rng() % 260) as usize;
            let mut d = vec![0u8; n];
            for b in d.iter_mut() { *b = (rng() & 0xFF) as u8; }
            let mlen = (rng() % 200) as usize;
            let mut m = vec![0u8; mlen];
            for b in m.iter_mut() { *b = (rng() & 0xFF) as u8; }
            let _ = ed25519_layout_ok(&d, &signer, &m);
        }
    }
}
