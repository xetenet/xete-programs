//! Shared settlement primitives used across every asset family: the taker's want-leg payment
//! (the one audited place fees are computed) and the rested-swap fill-load + validation.

use crate::cpi::*;
use crate::config::*;
use crate::royalty::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

/// A passed Token-Metadata `Metadata` account must be the CANONICAL PDA for `mint` AND owned by the
/// Token-Metadata program — so a settle can't be fed a spoofed/substituted metadata to redirect royalty.
pub(crate) fn verify_metadata_pda(metadata: &AccountInfo, mint: &AccountInfo) -> ProgramResult {
    if *metadata.owner != TOKEN_METADATA_ID {
        return Err(ProgramError::IllegalOwner);
    }
    let (pda, _) =
        Pubkey::find_program_address(&[b"metadata", TOKEN_METADATA_ID.as_ref(), mint.key.as_ref()], &TOKEN_METADATA_ID);
    if *metadata.key != pda {
        return Err(ProgramError::InvalidArgument);
    }
    Ok(())
}

/// The want-leg WITH the negotiated royalty, all funded from `taker_want`, fail-closed:
///   fee → fee_ata ; (want − fee − maker_share) → maker ; full royalty split across the asset's
///   on-chain creators by their shares.  maker_share is the maker's negotiated portion (skimmed from
///   their proceeds); taker_share = royalty − maker_share is charged ON TOP (so the taker pays
///   want + taker_share, bounded by max_want). Destination/shares/bps come ONLY from the asset metadata.
/// `royalty_mode == NONE` ⇒ behaves exactly like the plain want-leg (no metadata/creators needed).
#[allow(clippy::too_many_arguments)]
pub(crate) fn pay_want_and_royalty<'a>(
    program_id: &Pubkey,
    config: &AccountInfo,
    token_prog: &AccountInfo<'a>,
    taker_want: &AccountInfo<'a>,
    want_mint: &AccountInfo<'a>,
    fee_ata: &AccountInfo<'a>,
    maker_want: &AccountInfo<'a>,
    taker: &AccountInfo<'a>,
    want_amount: u64,
    max_want: u64,
    want_dec: u8,
    shard_seed: u8,
    royalty_mode: u8,
    maker_pct: u8,
    give_mint: &AccountInfo,
    metadata: Option<&AccountInfo>,
    creator_atas: &[AccountInfo<'a>],
) -> ProgramResult {
    let (fee_bps, _delist, fee_wallet, fee_shards) = read_config(program_id, config)?;
    let fee = ((want_amount as u128 * fee_bps as u128) / BPS_DENOM as u128) as u64;

    // Resolve the royalty + the maker's funded share from the asset metadata (chain = source of truth).
    let mut shares = [0u8; MAX_CREATORS];
    let mut creators = [Pubkey::new_from_array([0u8; 32]); MAX_CREATORS];
    let mut n = 0usize;
    let mut royalty = 0u64;
    let mut maker_share = 0u64;
    if royalty_mode != ROYALTY_NONE {
        let md_acct = metadata.ok_or(ProgramError::NotEnoughAccountKeys)?;
        verify_metadata_pda(md_acct, give_mint)?;
        {
            let data = md_acct.try_borrow_data()?;
            let (bps, cr, cnt) = parse_tm_royalty(&data)?;
            if bps == 0 || cnt == 0 {
                return Err(ProgramError::InvalidArgument); // term set but the asset declares no royalty
            }
            let mut sum = 0u32;
            for i in 0..cnt {
                creators[i] = cr[i].0;
                shares[i] = cr[i].1;
                sum += cr[i].1 as u32;
            }
            if sum != 100 {
                return Err(ProgramError::InvalidArgument); // shares must sum to exactly 100
            }
            n = cnt;
            royalty = ((want_amount as u128 * bps as u128) / BPS_DENOM as u128) as u64;
        }
        maker_share = match royalty_mode {
            ROYALTY_MAKER => royalty,
            ROYALTY_TAKER => 0,
            ROYALTY_SPLIT => {
                if maker_pct > 100 {
                    return Err(ProgramError::InvalidArgument);
                }
                ((royalty as u128 * maker_pct as u128) / 100u128) as u64
            }
            _ => return Err(ProgramError::InvalidArgument),
        };
        if creator_atas.len() != n {
            return Err(ProgramError::NotEnoughAccountKeys);
        }
        // each creator ATA must be the want-mint account of the matching on-chain creator
        for (i, ca) in creator_atas.iter().enumerate().take(n) {
            require_token_account(ca, want_mint.key, creators[i].as_ref())?;
        }
    }

    let taker_share = royalty - maker_share;
    let taker_total = want_amount.checked_add(taker_share).ok_or(ProgramError::ArithmeticOverflow)?;
    if taker_total > max_want {
        return Err(ProgramError::InvalidArgument); // taker's total spend exceeds their slippage cap
    }
    let maker_payment = want_amount
        .checked_sub(fee)
        .and_then(|x| x.checked_sub(maker_share))
        .ok_or(ProgramError::InsufficientFunds)?;

    // fee → fee wallet
    if fee > 0 {
        verify_fee_dest(program_id, fee_ata, want_mint, &fee_wallet, fee_shards, shard_seed)?;
        token_transfer(token_prog, taker_want, want_mint, fee_ata, taker, fee, want_dec, None)?;
    }
    // remainder → maker
    token_transfer(token_prog, taker_want, want_mint, maker_want, taker, maker_payment, want_dec, None)?;
    // full royalty → creators, dust-safe split (last gets the remainder)
    if royalty > 0 {
        let splits = split_by_share(royalty, &shares[..n]);
        for (i, ca) in creator_atas.iter().enumerate().take(n) {
            if splits[i] > 0 {
                token_transfer(token_prog, taker_want, want_mint, ca, taker, splits[i], want_dec, None)?;
            }
        }
    }
    Ok(())
}

/// Taker pays the want-leg: skim the protocol fee to the verified fee destination, the
/// remainder to the maker. The single place fees are computed, so no handler can drift.
/// `shard_seed` picks the fee-vault shard when sharding is on.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pay_want_leg<'a>(
    program_id: &Pubkey,
    config: &AccountInfo,
    token_prog: &AccountInfo<'a>,
    taker_want: &AccountInfo<'a>,
    want_mint: &AccountInfo<'a>,
    fee_ata: &AccountInfo<'a>,
    maker_want: &AccountInfo<'a>,
    taker: &AccountInfo<'a>,
    want_amount: u64,
    want_dec: u8,
    shard_seed: u8,
) -> ProgramResult {
    let (fee_bps, _delist, fee_wallet, fee_shards) = read_config(program_id, config)?;
    let fee = ((want_amount as u128 * fee_bps as u128) / BPS_DENOM as u128) as u64;
    if fee > 0 {
        verify_fee_dest(program_id, fee_ata, want_mint, &fee_wallet, fee_shards, shard_seed)?;
        token_transfer(token_prog, taker_want, want_mint, fee_ata, taker, fee, want_dec, None)?;
    }
    token_transfer(token_prog, taker_want, want_mint, maker_want, taker, want_amount - fee, want_dec, None)
}

/// BID coins-leg: the bidder's delegated coins -> the protocol fee + the owner's proceeds, signed by the
/// order PDA (the bidder's standing delegate). The mirror of pay_want_leg for the reverse (buy) direction:
/// same fee computation + destination verification, but the source is the bidder's ATA and the authority
/// is the PDA (seeds), not the taker.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pay_coins_via_delegate<'a>(
    program_id: &Pubkey,
    config: &AccountInfo,
    token_prog: &AccountInfo<'a>,
    maker_coins: &AccountInfo<'a>,
    coins_mint: &AccountInfo<'a>,
    fee_ata: &AccountInfo<'a>,
    owner_coins: &AccountInfo<'a>,
    order_auth: &AccountInfo<'a>,
    seeds: &[&[u8]],
    amount: u64,
    coins_dec: u8,
    shard_seed: u8,
) -> ProgramResult {
    let (fee_bps, _delist, fee_wallet, fee_shards) = read_config(program_id, config)?;
    let fee = ((amount as u128 * fee_bps as u128) / BPS_DENOM as u128) as u64;
    if fee > 0 {
        verify_fee_dest(program_id, fee_ata, coins_mint, &fee_wallet, fee_shards, shard_seed)?;
        token_transfer(token_prog, maker_coins, coins_mint, fee_ata, order_auth, fee, coins_dec, Some(seeds))?;
    }
    token_transfer(token_prog, maker_coins, coins_mint, owner_coins, order_auth, amount - fee, coins_dec, Some(seeds))
}



/// Load + validate a rested swap/listing (SWAP_LEN layout) at fill time: open status, the give/want
/// mints match, not expired, and — if the listing named a taker — that this is them. Returns
/// (maker, give_amount, want_amount, bump, royalty_mode, maker_pct). `give_key` is the give mint (or
/// the Core asset key). The royalty term is the negotiated PAYER stance; destination comes from the asset.
pub(crate) fn load_fill_state(
    state: &AccountInfo,
    give_key: &Pubkey,
    want_key: &Pubkey,
    taker_key: &Pubkey,
) -> Result<([u8; 32], u64, u64, u8, u8, u8), ProgramError> {
    let st = state.try_borrow_data()?;
    if st.len() != SWAP_LEN || st[S_STATUS] != STATUS_OPEN {
        return Err(ProgramError::InvalidAccountData);
    }
    if &st[S_GIVE_MINT..S_GIVE_MINT + 32] != give_key.as_ref()
        || &st[S_WANT_MINT..S_WANT_MINT + 32] != want_key.as_ref()
    {
        return Err(ProgramError::InvalidArgument);
    }
    let expiry = i64::from_le_bytes(st[S_EXPIRY..S_EXPIRY + 8].try_into().unwrap());
    if Clock::get()?.unix_timestamp >= expiry {
        return Err(ProgramError::InvalidArgument);
    }
    if st[S_TAKER..S_TAKER + 32].iter().any(|&b| b != 0) && &st[S_TAKER..S_TAKER + 32] != taker_key.as_ref() {
        return Err(ProgramError::InvalidArgument);
    }
    Ok((
        arr32(&st[S_MAKER..S_MAKER + 32])?,
        u64::from_le_bytes(st[S_GIVE_AMT..S_GIVE_AMT + 8].try_into().unwrap()),
        u64::from_le_bytes(st[S_WANT_AMT..S_WANT_AMT + 8].try_into().unwrap()),
        st[S_BUMP],
        st[S_ROYALTY_MODE],
        st[S_MAKER_PCT],
    ))
}
