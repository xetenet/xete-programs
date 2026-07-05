//! xete-swap — native Solana OTC settlement, hand-laid byte layout, auditable top to bottom.
//!
//! Written in native solana-program (no framework): what runs on chain is exactly what you read.
//! The crate is one domain per module; every instruction reads as straight-line business logic,
//! and all cross-program CPI lives behind the small helpers in `cpi`.
//!
//!   state       byte layouts + decoders          cpi      CPI wiring + account helpers
//!   config      fee policy / alias gate / shards  settle   shared want-leg + fill-load primitives
//!   swap        1:1 SPL swaps        (tags 0-3)    offer    escrowed bids        (tags 4-7)
//!   config tags fees / vaults        (tags 8-11)   basket   multi-mint + bearer  (tags 12-17)
//!   pnft        programmable NFTs    (tags 18-21)  mcore    Metaplex Core        (tags 22-25)
//!   escrowless  delegate listings   (tags 26-37)  order    sealed signed orders (tags 38-40)
//!
//! Security: see THREAT_MODEL.md for the attack/mitigation matrix.

mod cpi;
mod config;
mod settle;
mod state;
mod swap;
mod offer;
mod basket;
mod pnft;
mod mcore;
mod escrowless;
mod order;
mod royalty;
mod events;
// Re-exported ONLY so the host fuzz harness (fuzz/) can target the pure parser directly; visibility-only,
// no effect on the compiled .so or the on-chain (tag-dispatched) surface. See order::ed25519_layout_ok.
pub use order::ed25519_layout_ok;

use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey,
};

entrypoint!(process);

// On-chain program metadata (neodyme security.txt standard). Surfaced by Solana Explorer / Solscan so
// the program shows a name + contact instead of "unknown". Pure data section — no logic change. Gated
// off when built as a lib dependency (no-entrypoint) to avoid duplicate-symbol conflicts.
#[cfg(not(feature = "no-entrypoint"))]
solana_security_txt::security_txt! {
    name: "xete AXTSWAP",
    project_url: "https://xete.net",
    source_code: "https://github.com/xetenet/xete-swap",
    contacts: "link:https://xete.net",
    policy: "Escrowless + escrowed OTC settlement for SPL / Token-2022 / pNFT / Metaplex Core assets with negotiated, signature-bound royalties. Assets move only on maker-signed listings or maker-signed sealed orders; no admin path can move user assets. Report issues via https://xete.net; good-faith security research welcomed.",
    preferred_languages: "en",
    auditors: "Internal: 41-tag localnet integration matrix, host unit + fuzz (ed25519 layout 75M+, royalty parser 38M+), stateful sequence fuzz w/ value-conservation invariant, reproducible verified build. See DEPLOY_RUNBOOK.md."
}

fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    match tag {
        0 => swap::open_swap(program_id, accounts, rest),
        1 => swap::fill(program_id, accounts, rest),
        2 => swap::cancel_swap(program_id, accounts, rest),
        3 => swap::expire(program_id, accounts, rest),
        4 => offer::make_offer(program_id, accounts, rest),
        5 => offer::accept_offer(program_id, accounts, rest),
        6 => offer::withdraw_offer(program_id, accounts, rest),
        7 => offer::expire_offer(program_id, accounts, rest),
        8 => config::init_config(program_id, accounts, rest),
        9 => config::update_config(program_id, accounts, rest),
        10 => config::init_fee_vault(program_id, accounts, rest),
        11 => config::sweep_fees(program_id, accounts, rest),
        12 => basket::open_basket(program_id, accounts, rest),
        13 => basket::fill_basket(program_id, accounts, rest),
        14 => basket::claim_bearer(program_id, accounts, rest),
        15 => basket::rewrap(program_id, accounts, rest),
        16 => basket::cancel_basket(program_id, accounts, rest),
        17 => basket::expire_basket(program_id, accounts, rest),
        18 => pnft::open_pnft(program_id, accounts, rest),
        19 => pnft::fill_pnft(program_id, accounts, rest),
        20 => pnft::cancel_pnft(program_id, accounts, rest),
        21 => pnft::expire_pnft(program_id, accounts, rest),
        22 => mcore::open_core(program_id, accounts, rest),
        23 => mcore::fill_core(program_id, accounts, rest),
        24 => mcore::cancel_core(program_id, accounts, rest),
        25 => mcore::expire_core(program_id, accounts, rest),
        26 => escrowless::list(program_id, accounts, rest),
        27 => escrowless::fill_listing(program_id, accounts, rest),
        28 => escrowless::cancel_listing(program_id, accounts, rest),
        29 => escrowless::expire_listing(program_id, accounts, rest),
        30 => escrowless::list_pnft(program_id, accounts, rest),
        31 => escrowless::fill_listing_pnft(program_id, accounts, rest),
        32 => escrowless::cancel_listing_pnft(program_id, accounts, rest),
        33 => escrowless::expire_listing_pnft(program_id, accounts, rest),
        34 => escrowless::list_core(program_id, accounts, rest),
        35 => escrowless::fill_listing_core(program_id, accounts, rest),
        36 => escrowless::cancel_listing_core(program_id, accounts, rest),
        37 => escrowless::expire_listing_core(program_id, accounts, rest),
        38 => order::settle_signed_order(program_id, accounts, rest),
        39 => order::settle_signed_order_pnft(program_id, accounts, rest),
        40 => order::settle_signed_order_core(program_id, accounts, rest),
        41 => order::settle_signed_bid_spl(program_id, accounts, rest),
        42 => order::settle_signed_bid_pnft(program_id, accounts, rest),
        43 => order::settle_signed_bid_core(program_id, accounts, rest),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(fuzzing)]
pub use royalty::fuzz_parse_tm_royalty;
