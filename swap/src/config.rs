//! Fee-policy config singleton, the alias listing-gate, and fee-vault sharding.

use crate::cpi::*;
use crate::state::*;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError, pubkey::Pubkey,
};

// ---- 7e. init_config (tag 8) — create the fee-policy config (admin = signer) ----
// data: fee_wallet[32] | fee_bps u16 | delist_fee u64 | alias_program[32] | fee_shards u8  (75)
// accounts: admin(signer/payer) | config(pda) | system
pub(crate) fn init_config(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 75 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if u16::from_le_bytes(d[32..34].try_into().unwrap()) > BPS_DENOM as u16 {
        return Err(ProgramError::InvalidArgument); // fee can't exceed 100%
    }
    let [admin, config, system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !admin.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (cfg_pda, bump) = Pubkey::find_program_address(&[b"config"], program_id);
    if &cfg_pda != config.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [bump];
    create_pda(admin, config, system, CONFIG_LEN, program_id, &[b"config", &bs])?;
    let mut c = config.try_borrow_mut_data()?;
    c[C_ADMIN..C_ADMIN + 32].copy_from_slice(admin.key.as_ref());
    c[C_FEE_WALLET..C_FEE_WALLET + 32].copy_from_slice(&d[0..32]);
    c[C_FEE_BPS..C_FEE_BPS + 2].copy_from_slice(&d[32..34]);
    c[C_DELIST_FEE..C_DELIST_FEE + 8].copy_from_slice(&d[34..42]);
    c[C_ALIAS_PROGRAM..C_ALIAS_PROGRAM + 32].copy_from_slice(&d[42..74]);
    c[C_FEE_SHARDS] = d[74];
    c[C_BUMP] = bump;
    Ok(())
}

// ---- 7f. update_config (tag 9) — retune fees / fee-wallet (admin only) ----
// data: fee_wallet[32] | fee_bps u16 | delist_fee u64 | alias_program[32] | fee_shards u8  (75)
// accounts: admin(signer) | config(pda)
pub(crate) fn update_config(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 75 {
        return Err(ProgramError::InvalidInstructionData);
    }
    if u16::from_le_bytes(d[32..34].try_into().unwrap()) > BPS_DENOM as u16 {
        return Err(ProgramError::InvalidArgument); // fee can't exceed 100%
    }
    let [admin, config] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !admin.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if config.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (cfg_pda, _) = Pubkey::find_program_address(&[b"config"], program_id);
    if &cfg_pda != config.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let mut c = config.try_borrow_mut_data()?;
    if c.len() != CONFIG_LEN || &c[C_ADMIN..C_ADMIN + 32] != admin.key.as_ref() {
        return Err(ProgramError::InvalidArgument); // only the admin may update
    }
    c[C_FEE_WALLET..C_FEE_WALLET + 32].copy_from_slice(&d[0..32]);
    c[C_FEE_BPS..C_FEE_BPS + 2].copy_from_slice(&d[32..34]);
    c[C_DELIST_FEE..C_DELIST_FEE + 8].copy_from_slice(&d[34..42]);
    c[C_ALIAS_PROGRAM..C_ALIAS_PROGRAM + 32].copy_from_slice(&d[42..74]);
    c[C_FEE_SHARDS] = d[74];
    Ok(())
}

// ---- 7g. init_fee_vault (tag 10) — create a program-owned fee-vault for one (want_mint, shard) ----
// data: shard_index u8
// accounts: payer(signer) | fee_vault(pda) | want_mint | token_program | system
// The vault's token authority is the config PDA, so only `sweep_fees` can drain it.
pub(crate) fn init_fee_vault(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let idx = d[0];
    let [payer, fee_vault, want_mint, token_program, system] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (cfg_pda, _) = Pubkey::find_program_address(&[b"config"], program_id);
    let (fv_pda, fbump) = Pubkey::find_program_address(&[b"fee_vault", want_mint.key.as_ref(), &[idx]], program_id);
    if &fv_pda != fee_vault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let bs = [fbump];
    let ix = [idx];
    create_pda(payer, fee_vault, system, TOKEN_ACCT_LEN, &spl_token::ID, &[b"fee_vault", want_mint.key.as_ref(), &ix, &bs])?;
    // token authority = the config PDA, so fees can only be moved by sweep_fees (config-signed)
    token_init(token_program, fee_vault, want_mint, &cfg_pda)?;
    Ok(())
}

// ---- 7h. sweep_fees (tag 11) — drain one fee-vault into the configured fee wallet's ATA (permissionless) ----
// data: shard_index u8
// accounts: caller(signer/payer) | fee_vault(pda) | want_mint | config(pda) | fee_wallet_ata | token_program
pub(crate) fn sweep_fees(program_id: &Pubkey, accounts: &[AccountInfo], d: &[u8]) -> ProgramResult {
    if d.len() != 1 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let idx = d[0];
    let [caller, fee_vault, want_mint, config, fee_wallet_ata, token_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !caller.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (fv_pda, _) = Pubkey::find_program_address(&[b"fee_vault", want_mint.key.as_ref(), &[idx]], program_id);
    if &fv_pda != fee_vault.key {
        return Err(ProgramError::InvalidSeeds);
    }
    if config.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (cfg_pda, cfg_bump) = Pubkey::find_program_address(&[b"config"], program_id);
    if &cfg_pda != config.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let fee_wallet = {
        let c = config.try_borrow_data()?;
        if c.len() != CONFIG_LEN {
            return Err(ProgramError::InvalidAccountData);
        }
        arr32(&c[C_FEE_WALLET..C_FEE_WALLET + 32])?
    };
    // destination must be the configured fee wallet's want-mint account
    require_token_account(fee_wallet_ata, want_mint.key, &fee_wallet)?;
    // amount = the vault's full token balance (SPL token amount @ offset 64)
    let amount = {
        let v = fee_vault.try_borrow_data()?;
        if v.len() < 72 {
            return Err(ProgramError::InvalidAccountData);
        }
        u64::from_le_bytes(v[64..72].try_into().unwrap())
    };
    if amount == 0 {
        return Ok(()); // nothing to sweep
    }
    let want_dec = mint_decimals(want_mint)?;
    // the config PDA is the vault's token authority -> it signs the transfer out
    let bs = [cfg_bump];
    let cfg_seeds: &[&[u8]] = &[b"config", &bs];
    token_transfer(token_program, fee_vault, want_mint, fee_wallet_ata, config, amount, want_dec, Some(cfg_seeds))?;
    Ok(())
}

/// Read (fee_bps, delist_fee, fee_wallet, fee_shards) from the singleton config PDA, verifying it.
pub(crate) fn read_config(program_id: &Pubkey, config: &AccountInfo) -> Result<(u16, u64, [u8; 32], u8), ProgramError> {
    if config.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let (cfg_pda, _) = Pubkey::find_program_address(&[b"config"], program_id);
    if &cfg_pda != config.key {
        return Err(ProgramError::InvalidSeeds);
    }
    let c = config.try_borrow_data()?;
    if c.len() != CONFIG_LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok((
        u16::from_le_bytes(c[C_FEE_BPS..C_FEE_BPS + 2].try_into().unwrap()),
        u64::from_le_bytes(c[C_DELIST_FEE..C_DELIST_FEE + 8].try_into().unwrap()),
        arr32(&c[C_FEE_WALLET..C_FEE_WALLET + 32])?,
        c[C_FEE_SHARDS],
    ))
}

/// Verify the fee-destination token account passed by the caller is legitimate.
/// `fee_shards == 0`: must be the fee wallet's want-mint account (single-wallet, the default).
/// `fee_shards  > 0`: must be the program-owned fee-vault PDA ["fee_vault", want_mint, idx],
/// where idx = shard_seed % fee_shards.
pub(crate) fn verify_fee_dest(
    program_id: &Pubkey,
    fee_ata: &AccountInfo,
    want_mint: &AccountInfo,
    fee_wallet: &[u8; 32],
    fee_shards: u8,
    shard_seed: u8,
) -> ProgramResult {
    if fee_shards == 0 {
        require_token_account(fee_ata, want_mint.key, fee_wallet.as_ref())
    } else {
        let idx = shard_seed % fee_shards;
        let (fv, _) = Pubkey::find_program_address(&[b"fee_vault", want_mint.key.as_ref(), &[idx]], program_id);
        if &fv != fee_ata.key {
            return Err(ProgramError::InvalidArgument); // fee must land in the derived shard vault
        }
        Ok(())
    }
}

/// Listing gate: if config names an alias program, `caller` must present their Alias account
/// (owned by that program, with its owner field == caller). All-zero alias program = gate off.
pub(crate) fn require_alias(program_id: &Pubkey, config: &AccountInfo, caller: &[u8], alias: &AccountInfo) -> ProgramResult {
    let alias_program = {
        if config.owner != program_id {
            return Err(ProgramError::IllegalOwner);
        }
        let (cfg_pda, _) = Pubkey::find_program_address(&[b"config"], program_id);
        if &cfg_pda != config.key {
            return Err(ProgramError::InvalidSeeds);
        }
        let c = config.try_borrow_data()?;
        if c.len() != CONFIG_LEN {
            return Err(ProgramError::InvalidAccountData);
        }
        arr32(&c[C_ALIAS_PROGRAM..C_ALIAS_PROGRAM + 32])?
    };
    if alias_program.iter().all(|&b| b == 0) {
        return Ok(()); // gate disabled
    }
    if alias.owner.as_ref() != alias_program.as_ref() {
        return Err(ProgramError::InvalidArgument); // not an account of the alias program
    }
    let a = alias.try_borrow_data()?;
    if a.len() != ALIAS_LEN || &a[0..32] != caller {
        return Err(ProgramError::InvalidArgument); // alias not owned by the caller
    }
    Ok(())
}
