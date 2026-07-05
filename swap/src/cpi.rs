//! CPI wiring + low-level account helpers (the only place that talks to other programs).

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, instruction::{AccountMeta, Instruction},
    program::{invoke, invoke_signed}, program_error::ProgramError, pubkey::Pubkey, rent::Rent,
    sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

// -- foreign programs we CPI into (one manifest; each is re-verified at its call site) --
pub(crate) const TOKEN_2022_ID: Pubkey = solana_program::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub(crate) const TOKEN_METADATA_ID: Pubkey = solana_program::pubkey!("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
pub(crate) const SYSVAR_INSTRUCTIONS_ID: Pubkey = solana_program::pubkey!("Sysvar1nstructions1111111111111111111111111");
pub(crate) const ATA_PROGRAM_ID: Pubkey = solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub(crate) const MPL_CORE_ID: Pubkey = solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Create a program-derived account, PDA-signed by `seeds`.
pub(crate) fn create_pda<'a>(
    payer: &AccountInfo<'a>,
    acct: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    space: usize,
    owner: &Pubkey,
    seeds: &[&[u8]],
) -> ProgramResult {
    let lamports = Rent::get()?.minimum_balance(space);
    invoke_signed(
        &system_instruction::create_account(payer.key, acct.key, lamports, space as u64, owner),
        &[payer.clone(), acct.clone(), system.clone()],
        &[seeds],
    )
}

/// The give-leg token program must be SPL Token or Token-2022 AND must actually own the give mint
/// (no spoofing the program to redirect the escrow CPI).
pub(crate) fn assert_token_program(token_prog: &AccountInfo, give_mint: &AccountInfo) -> ProgramResult {
    let tp = *token_prog.key;
    if tp != spl_token::ID && tp != TOKEN_2022_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if give_mint.owner != &tp {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

/// Associated token account for (owner, mint) under the classic SPL Token program.
pub(crate) fn ata_for(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[owner.as_ref(), spl_token::ID.as_ref(), mint.as_ref()], &ATA_PROGRAM_ID).0
}

/// CPI mpl-token-metadata `TransferV1(amount)`. Account order byte-exact to the captured blueprint.
/// `signer_seeds = Some(..)` when `authority` is the swap PDA (vault release); `None` for a maker signer (escrow).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mpl_transfer<'a>(
    tm: &AccountInfo<'a>, src_ta: &AccountInfo<'a>, token_owner: &AccountInfo<'a>,
    dst_ta: &AccountInfo<'a>, dst_owner: &AccountInfo<'a>, mint: &AccountInfo<'a>,
    metadata: &AccountInfo<'a>, edition: &AccountInfo<'a>, owner_record: &AccountInfo<'a>,
    dst_record: &AccountInfo<'a>, authority: &AccountInfo<'a>, payer: &AccountInfo<'a>,
    system: &AccountInfo<'a>, sysvar_ix: &AccountInfo<'a>, spl_tok: &AccountInfo<'a>,
    ata_prog: &AccountInfo<'a>, rules_prog: &AccountInfo<'a>, rules: &AccountInfo<'a>,
    amount: u64, signer_seeds: Option<&[&[u8]]>,
) -> ProgramResult {
    let mut data = Vec::with_capacity(11);
    data.push(0x31u8); // Transfer
    data.push(0x00u8); // V1
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(0x00u8); // authorization_data: None
    let metas = vec![
        AccountMeta::new(*src_ta.key, false),
        AccountMeta::new_readonly(*token_owner.key, false),
        AccountMeta::new(*dst_ta.key, false),
        AccountMeta::new_readonly(*dst_owner.key, false),
        AccountMeta::new_readonly(*mint.key, false),
        AccountMeta::new(*metadata.key, false),
        AccountMeta::new_readonly(*edition.key, false),
        AccountMeta::new(*owner_record.key, false),
        AccountMeta::new(*dst_record.key, false),
        AccountMeta::new_readonly(*authority.key, true),
        AccountMeta::new(*payer.key, true),
        AccountMeta::new_readonly(*system.key, false),
        AccountMeta::new_readonly(*sysvar_ix.key, false),
        AccountMeta::new_readonly(*spl_tok.key, false),
        AccountMeta::new_readonly(*ata_prog.key, false),
        AccountMeta::new_readonly(*rules_prog.key, false),
        AccountMeta::new_readonly(*rules.key, false),
    ];
    let ix = Instruction { program_id: *tm.key, accounts: metas, data };
    let infos = [
        src_ta.clone(), token_owner.clone(), dst_ta.clone(), dst_owner.clone(), mint.clone(),
        metadata.clone(), edition.clone(), owner_record.clone(), dst_record.clone(), authority.clone(),
        payer.clone(), system.clone(), sysvar_ix.clone(), spl_tok.clone(), ata_prog.clone(),
        rules_prog.clone(), rules.clone(), tm.clone(),
    ];
    match signer_seeds {
        Some(seeds) => invoke_signed(&ix, &infos, &[seeds]),
        None => invoke(&ix, &infos),
    }
}

/// CPI mpl-core `TransferV1`. data = [0x0e Transfer][0x00 compressionProof=None]; account order per the blueprint.
/// `authority` is the sentinel (`core`) on escrow (defaults to payer/owner) or the swap PDA on release.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mpl_core_transfer<'a>(
    core: &AccountInfo<'a>, asset: &AccountInfo<'a>, collection: &AccountInfo<'a>, payer: &AccountInfo<'a>,
    authority: &AccountInfo<'a>, new_owner: &AccountInfo<'a>, authority_is_signer: bool,
    signer_seeds: Option<&[&[u8]]>,
) -> ProgramResult {
    let data = vec![0x0eu8, 0x00u8];
    let metas = vec![
        AccountMeta::new(*asset.key, false),
        AccountMeta::new_readonly(*collection.key, false),
        AccountMeta::new(*payer.key, true),
        AccountMeta::new_readonly(*authority.key, authority_is_signer),
        AccountMeta::new_readonly(*new_owner.key, false),
        AccountMeta::new_readonly(*core.key, false), // system slot: None sentinel = core id
        AccountMeta::new_readonly(*core.key, false), // log_wrapper slot: None sentinel = core id
    ];
    let ix = Instruction { program_id: *core.key, accounts: metas, data };
    let infos = [asset.clone(), collection.clone(), payer.clone(), authority.clone(), new_owner.clone(), core.clone()];
    match signer_seeds {
        Some(seeds) => invoke_signed(&ix, &infos, &[seeds]),
        None => invoke(&ix, &infos),
    }
}

/// CPI Token-Metadata `DelegateTransferV1(amount)` (0x2c/0x02) or `RevokeTransferV1` (0x2d/0x02).
/// 14-account layout per the captured blueprint; authority (owner) is a tx signer. `revoke=true` swaps the opcode.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mpl_delegate_or_revoke<'a>(
    tm: &AccountInfo<'a>, token_record: &AccountInfo<'a>, delegate: &AccountInfo<'a>, metadata: &AccountInfo<'a>,
    edition: &AccountInfo<'a>, mint: &AccountInfo<'a>, owner_ata: &AccountInfo<'a>, authority: &AccountInfo<'a>,
    payer: &AccountInfo<'a>, system: &AccountInfo<'a>, sysvar_ix: &AccountInfo<'a>, spl_tok: &AccountInfo<'a>,
    rules_prog: &AccountInfo<'a>, rules: &AccountInfo<'a>, amount: u64, revoke: bool,
) -> ProgramResult {
    let mut data = Vec::with_capacity(11);
    if revoke {
        data.push(0x2du8); data.push(0x02u8); // Revoke / TransferV1
    } else {
        data.push(0x2cu8); data.push(0x02u8); // Delegate / TransferV1
        data.extend_from_slice(&amount.to_le_bytes());
        data.push(0x00u8); // authorization_data: None
    }
    let metas = vec![
        AccountMeta::new(*token_record.key, false),       // 0 delegate_record (= token_record for pNFT)
        AccountMeta::new_readonly(*delegate.key, false),  // 1 delegate
        AccountMeta::new(*metadata.key, false),           // 2 metadata
        AccountMeta::new_readonly(*edition.key, false),   // 3 master_edition
        AccountMeta::new(*token_record.key, false),       // 4 token_record
        AccountMeta::new_readonly(*mint.key, false),      // 5 mint
        AccountMeta::new(*owner_ata.key, false),          // 6 token
        AccountMeta::new_readonly(*authority.key, true),  // 7 authority (owner, signer)
        AccountMeta::new(*payer.key, true),               // 8 payer (signer)
        AccountMeta::new_readonly(*system.key, false),
        AccountMeta::new_readonly(*sysvar_ix.key, false),
        AccountMeta::new_readonly(*spl_tok.key, false),
        AccountMeta::new_readonly(*rules_prog.key, false),
        AccountMeta::new_readonly(*rules.key, false),
    ];
    let ix = Instruction { program_id: *tm.key, accounts: metas, data };
    let infos = [
        token_record.clone(), delegate.clone(), metadata.clone(), edition.clone(), mint.clone(), owner_ata.clone(),
        authority.clone(), payer.clone(), system.clone(), sysvar_ix.clone(), spl_tok.clone(), rules_prog.clone(),
        rules.clone(), tm.clone(),
    ];
    invoke(&ix, &infos)
}

/// CPI an mpl-core plugin instruction (AddPlugin/ApprovePluginAuthority/RemovePlugin) — shared 6-account layout
/// (asset, collection=None, signer/payer, authority=None, system, log_wrapper=None). `seeds` = Some when the signer
/// is the listing PDA (release/revoke), None when it is the maker (list).
pub(crate) fn mpl_core_plugin<'a>(
    core: &AccountInfo<'a>, asset: &AccountInfo<'a>, collection: &AccountInfo<'a>, signer: &AccountInfo<'a>,
    system: &AccountInfo<'a>, data: Vec<u8>, seeds: Option<&[&[u8]]>,
) -> ProgramResult {
    // Collection slot: the None sentinel (= the core program id) for standalone assets; the REAL collection
    // account — WRITABLE, per the captured blueprint — for collection-member assets (mpl-core rejects member
    // plugin ops without it: MissingCollection / 0x19, the Velvetfur class).
    let coll_meta = if collection.key == core.key {
        AccountMeta::new_readonly(*core.key, false)
    } else {
        AccountMeta::new(*collection.key, false)
    };
    let metas = vec![
        AccountMeta::new(*asset.key, false),
        coll_meta,
        AccountMeta::new(*signer.key, true),          // payer + authority (signer / PDA-signed)
        AccountMeta::new_readonly(*core.key, false),  // authority (None -> defaults to signer)
        AccountMeta::new_readonly(*system.key, false),
        AccountMeta::new_readonly(*core.key, false),  // log_wrapper (None sentinel)
    ];
    let ix = Instruction { program_id: *core.key, accounts: metas, data };
    let infos = [asset.clone(), collection.clone(), signer.clone(), system.clone(), core.clone()];
    match seeds {
        Some(s) => invoke_signed(&ix, &infos, &[s]),
        None => invoke(&ix, &infos),
    }
}

/// SPL `approve` — the token owner (`authority`, a tx signer) delegates `amount` to `delegate`.
pub(crate) fn token_approve<'a>(token_prog: &AccountInfo<'a>, source: &AccountInfo<'a>, delegate: &AccountInfo<'a>, authority: &AccountInfo<'a>, amount: u64) -> ProgramResult {
    let mut ix = spl_token::instruction::approve(&spl_token::ID, source.key, delegate.key, authority.key, &[], amount)?;
    ix.program_id = *token_prog.key;
    invoke(&ix, &[source.clone(), delegate.clone(), authority.clone(), token_prog.clone()])
}

/// SPL `revoke` — the token owner clears any delegate on their account.
pub(crate) fn token_revoke<'a>(token_prog: &AccountInfo<'a>, source: &AccountInfo<'a>, authority: &AccountInfo<'a>) -> ProgramResult {
    let mut ix = spl_token::instruction::revoke(&spl_token::ID, source.key, authority.key, &[])?;
    ix.program_id = *token_prog.key;
    invoke(&ix, &[source.clone(), authority.clone(), token_prog.clone()])
}

/// Initialize an SPL token account; `authority` becomes its token owner.
pub(crate) fn token_init<'a>(token_prog: &AccountInfo<'a>, acct: &AccountInfo<'a>, mint: &AccountInfo<'a>, authority: &Pubkey) -> ProgramResult {
    let mut ix = spl_token::instruction::initialize_account3(&spl_token::ID, acct.key, mint.key, authority)?;
    ix.program_id = *token_prog.key;
    invoke(&ix, &[acct.clone(), mint.clone(), token_prog.clone()])
}

/// SPL `transfer_checked`. `seeds = Some(..)` when `authority` is a PDA (vault release);
/// `None` when `authority` is a transaction signer (deposit).
pub(crate) fn token_transfer<'a>(
    token_prog: &AccountInfo<'a>,
    source: &AccountInfo<'a>,
    mint: &AccountInfo<'a>,
    dest: &AccountInfo<'a>,
    authority: &AccountInfo<'a>,
    amount: u64,
    decimals: u8,
    seeds: Option<&[&[u8]]>,
) -> ProgramResult {
    let mut ix = spl_token::instruction::transfer_checked(
        &spl_token::ID, source.key, mint.key, dest.key, authority.key, &[], amount, decimals,
    )?;
    ix.program_id = *token_prog.key;
    let accts = [source.clone(), mint.clone(), dest.clone(), authority.clone(), token_prog.clone()];
    match seeds {
        Some(s) => invoke_signed(&ix, &accts, &[s]),
        None => invoke(&ix, &accts),
    }
}

/// SPL `close_account`, PDA-signed by `seeds` (the vault authority is always a PDA here).
pub(crate) fn token_close<'a>(token_prog: &AccountInfo<'a>, acct: &AccountInfo<'a>, dest: &AccountInfo<'a>, authority: &AccountInfo<'a>, seeds: &[&[u8]]) -> ProgramResult {
    let mut ix = spl_token::instruction::close_account(&spl_token::ID, acct.key, dest.key, authority.key, &[])?;
    ix.program_id = *token_prog.key;
    invoke_signed(&ix, &[acct.clone(), dest.clone(), authority.clone(), token_prog.clone()], &[seeds])
}

/// A passed token account must be `mint`-denominated and owned by `owner` (no redirection).
pub(crate) fn require_token_account(acct: &AccountInfo, mint: &Pubkey, owner: &[u8]) -> ProgramResult {
    let a = acct.try_borrow_data()?;
    if a.len() < 64 || &a[0..32] != mint.as_ref() || &a[32..64] != owner {
        return Err(ProgramError::InvalidArgument);
    }
    Ok(())
}

/// Sweep every lamport from `account` into `dest` and wipe its data.
pub(crate) fn close(account: &AccountInfo, dest: &AccountInfo) -> ProgramResult {
    let amt = **account.lamports.borrow();
    let new_dest = (**dest.lamports.borrow()).checked_add(amt).ok_or(ProgramError::ArithmeticOverflow)?;
    **dest.lamports.borrow_mut() = new_dest;
    **account.lamports.borrow_mut() = 0;
    account.try_borrow_mut_data()?.fill(0);
    Ok(())
}
