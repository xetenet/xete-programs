//! Account byte layouts + small decoders shared by every instruction.

use solana_program::{account_info::AccountInfo, program_error::ProgramError};

// ---- 2. Swap account layout (218 bytes) ----
pub(crate) const S_MAKER: usize = 0; //      [u8;32]
pub(crate) const S_GIVE_MINT: usize = 32; //   [u8;32]
pub(crate) const S_GIVE_AMT: usize = 64; //    u64 LE
pub(crate) const S_WANT_MINT: usize = 72; //   [u8;32]
pub(crate) const S_WANT_AMT: usize = 104; //   u64 LE
pub(crate) const S_TAKER: usize = 112; //      [u8;32]  named taker for a private swap (all-zero = open to any)
pub(crate) const S_TERMS: usize = 144; //      [u8;32]  H = hash(terms || item)
pub(crate) const S_EXPIRY: usize = 176; //     i64 LE
pub(crate) const S_STATUS: usize = 184; //     u8  (0=Open, 1=Filled, 2=Cancelled)
pub(crate) const S_BUMP: usize = 185; //       u8
pub(crate) const S_NONCE: usize = 186; //      [u8;32]  the open nonce, published so a discovered listing is fillable
pub(crate) const S_ROYALTY_MODE: usize = 218; // u8  royalty payer stance: 0=NONE 1=MAKER 2=TAKER 3=SPLIT
pub(crate) const S_MAKER_PCT: usize = 219; //    u8  0..=100 — maker-funded % of the royalty when mode==SPLIT
pub(crate) const SWAP_LEN: usize = 220;
// Royalty payer modes (S_ROYALTY_MODE). Destination + shares + bps come from the asset's on-chain
// metadata at settle (see royalty.rs) — never stored here; only the negotiated PAYER stance is.
pub(crate) const ROYALTY_NONE: u8 = 0;
pub(crate) const ROYALTY_MAKER: u8 = 1; // maker pays the full royalty from proceeds
pub(crate) const ROYALTY_TAKER: u8 = 2; // taker pays the full royalty on top
pub(crate) const ROYALTY_SPLIT: u8 = 3; // maker pays maker_pct%, taker pays the rest
pub(crate) const TOKEN_ACCT_LEN: usize = 165; // SPL Token account size

pub(crate) const STATUS_OPEN: u8 = 0;

// ---- Offer account layout (186 bytes) — a price bid on an open swap ----
pub(crate) const O_OFFERER: usize = 0; //      [u8;32]
pub(crate) const O_SWAP: usize = 32; //        [u8;32]  the swap PDA this offer bids on
pub(crate) const O_WANT_MINT: usize = 64; //   [u8;32]  currency escrowed (= swap.want_mint)
pub(crate) const O_WANT_AMT: usize = 96; //    u64 LE   the bid amount (escrowed in ovault)
pub(crate) const O_GIVE_MINT: usize = 104; //  [u8;32]  the goods this bid is FOR (swap.give_mint at bid time)
pub(crate) const O_GIVE_AMT: usize = 136; //   u64 LE   amount of goods the bid expects to receive
pub(crate) const O_EXPIRY: usize = 144; //     i64 LE
pub(crate) const O_STATUS: usize = 152; //     u8
pub(crate) const O_BUMP: usize = 153; //       u8
pub(crate) const O_NONCE: usize = 154; //      [u8;32]  the offer nonce, published so a discovered bid is acceptable
pub(crate) const OFFER_LEN: usize = 186;

// ---- Config account layout (108 bytes) — singleton PDA ["config"]; fee policy, tunable by admin ----
pub(crate) const C_ADMIN: usize = 0; //        [u8;32]  the only key that may update this config
pub(crate) const C_FEE_WALLET: usize = 32; //  [u8;32]  fee destination (fees land in its want-mint ATA)
pub(crate) const C_FEE_BPS: usize = 64; //     u16 LE   settlement fee in basis points (30 = 0.30%)
pub(crate) const C_DELIST_FEE: usize = 66; //  u64 LE   flat lamports charged to the maker on a voluntary cancel
pub(crate) const C_ALIAS_PROGRAM: usize = 74; //[u8;32]  the alias-registry program; all-zero = listing gate OFF
pub(crate) const C_FEE_SHARDS: usize = 106; // u8        fee-vault shard count; 0 = single fee wallet (sharding OFF)
pub(crate) const C_BUMP: usize = 107; //       u8
pub(crate) const CONFIG_LEN: usize = 108;
pub(crate) const BPS_DENOM: u64 = 10_000;
pub(crate) const ALIAS_LEN: usize = 106; //    xete-alias Alias account size (owner field at offset 0)

// ---- 2b. BASKET settlement layout (additive; tags 12+). 1:1 paths above are unchanged. ----
// A basket = up to MAX_LEG entries of {mint[32] + amount u64[8]} = ENTRY bytes. Generalizes the
// single give/want into bounded lists so tokens, 1 NFT, up-to-5 NFTs, and mixed are ONE code path.
pub(crate) const MAX_LEG: usize = 5;
pub(crate) const ENTRY: usize = 40; //         mint[32] + amount u64[8]
pub(crate) const B_MAKER: usize = 0; //        [u8;32]
pub(crate) const B_GIVE_COUNT: usize = 32; //  u8  (1..=5)
pub(crate) const B_WANT_COUNT: usize = 33; //  u8  (0..=5; 0 = open-to-offers / accept-a-bid)
pub(crate) const B_GIVE: usize = 34; //        MAX_LEG * ENTRY (34..234)  give basket; first GIVE_COUNT valid
pub(crate) const B_WANT: usize = 234; //       MAX_LEG * ENTRY (234..434) want basket; first WANT_COUNT valid
pub(crate) const B_TAKER: usize = 434; //      [u8;32]  named taker (all-zero = open to any)
pub(crate) const B_AUTH_MODE: usize = 466; //  u8  0 = normal (taker/maker), 1 = bearer (claim-by-key)
pub(crate) const B_CLAIM_AUTH: usize = 467; // [u8;32]  hash(bearer_pubkey) for bearer; zero otherwise (token<->item unlinkable)
pub(crate) const B_EXPIRY: usize = 499; //     i64 LE
pub(crate) const B_STATUS: usize = 507; //     u8  (0 Open / 1 Filled / 2 Cancelled / 3 Claimed)
pub(crate) const B_BUMP: usize = 508; //       u8
pub(crate) const B_NONCE: usize = 509; //      [u8;32]
pub(crate) const BASKET_LEN: usize = 541;
pub(crate) const AUTH_NORMAL: u8 = 0;
pub(crate) const AUTH_BEARER: u8 = 1;

pub(crate) fn arr32(b: &[u8]) -> Result<[u8; 32], ProgramError> {
    b.try_into().map_err(|_| ProgramError::InvalidInstructionData)
}

/// Mint decimals live at offset 44 of an SPL Mint account.
pub(crate) fn mint_decimals(mint: &AccountInfo) -> Result<u8, ProgramError> {
    mint.try_borrow_data()?
        .get(44)
        .copied()
        .ok_or(ProgramError::InvalidAccountData)
}
