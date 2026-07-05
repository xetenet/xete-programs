//! Public, indexer-friendly events emitted on every order creation.
//!
//! Wire format is the de-facto Solana standard (Anchor `Program data:` logs): a `sol_log_data` call
//! carrying an 8-byte discriminator followed by Borsh-serialized fields. Any tool that already decodes
//! Anchor events — Helius, Solscan, Dune decoders, @coral-xyz/anchor — consumes it with only our published
//! IDL (idl/xete_swap_events.json); no bespoke work. Every field here is fixed-size, so the Borsh encoding
//! is exactly the field-order byte concatenation in `encode_post_created` (documented 1:1 in the IDL).
//!
//! BRAND STAMP (origin, four ways): (1) the event name `XetePostCreated` namespaces the discriminator to
//! xete; (2) an explicit ASCII `xete` magic is the first field; (3) a schema `version` follows; (4) the
//! emitting program id (AXTSWAP…) is the implicit fourth stamp. NOTE: every field here is ALSO public in
//! the created account, so this log is a discovery/notification convenience — never a confidentiality
//! boundary (see the confidential-custody track if hiding is ever a goal).

use solana_program::{log::sol_log_data, pubkey::Pubkey};

/// First 8 bytes of sha256("event:XetePostCreated") — the Anchor-standard event discriminator.
const DISC_POST_CREATED: [u8; 8] = [0x51, 0x55, 0xd0, 0xc6, 0x8d, 0xfd, 0x67, 0xfd];
const MAGIC: &[u8; 4] = b"xete";
const SCHEMA_VERSION: u8 = 1;
const POST_CREATED_LEN: usize = 8 + 4 + 1 + 1 + 32 + 32 + 32 + 32 + 8 + 8 + 8 + 32; // 198

/// `kind` values (documented in the IDL). Escrow = give-leg held in a program vault; listing = escrowless.
pub(crate) const KIND_SWAP: u8 = 0; //          fungible escrow swap        (tag 0)
pub(crate) const KIND_LISTING: u8 = 1; //       escrowless SPL listing      (tag 26)
pub(crate) const KIND_LISTING_PNFT: u8 = 2; //  escrowless pNFT listing     (tag 30)
pub(crate) const KIND_LISTING_CORE: u8 = 3; //  escrowless Core listing     (tag 34)
pub(crate) const KIND_SWAP_PNFT: u8 = 4; //     pNFT escrow swap            (tag 18)
pub(crate) const KIND_SWAP_CORE: u8 = 5; //     Core escrow swap            (tag 22)

/// Pure, allocation-free serializer for `XetePostCreated`. Split from the syscall so the exact wire bytes
/// (which the published IDL commits to) are unit-testable on the host. `post` = the created PDA; `give`/`want`
/// are the asset mints (for Core, the asset address).
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_post_created(
    kind: u8,
    maker: &Pubkey,
    post: &Pubkey,
    give: &Pubkey,
    want: &Pubkey,
    give_amount: u64,
    want_amount: u64,
    expiry: i64,
    nonce: &[u8; 32],
) -> [u8; POST_CREATED_LEN] {
    let mut b = [0u8; POST_CREATED_LEN];
    b[0..8].copy_from_slice(&DISC_POST_CREATED);
    b[8..12].copy_from_slice(MAGIC);
    b[12] = SCHEMA_VERSION;
    b[13] = kind;
    b[14..46].copy_from_slice(maker.as_ref());
    b[46..78].copy_from_slice(post.as_ref());
    b[78..110].copy_from_slice(give.as_ref());
    b[110..142].copy_from_slice(want.as_ref());
    b[142..150].copy_from_slice(&give_amount.to_le_bytes());
    b[150..158].copy_from_slice(&want_amount.to_le_bytes());
    b[158..166].copy_from_slice(&expiry.to_le_bytes());
    b[166..198].copy_from_slice(nonce);
    b
}

/// Emit `XetePostCreated` for a newly created, fillable order. Emitted only on the success path — a later
/// revert drops the log with the rest of the transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_post_created(
    kind: u8,
    maker: &Pubkey,
    post: &Pubkey,
    give: &Pubkey,
    want: &Pubkey,
    give_amount: u64,
    want_amount: u64,
    expiry: i64,
    nonce: &[u8; 32],
) {
    let buf = encode_post_created(kind, maker, post, give, want, give_amount, want_amount, expiry, nonce);
    sol_log_data(&[&buf]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_layout_is_stable() {
        let maker = Pubkey::new_from_array([1u8; 32]);
        let post = Pubkey::new_from_array([2u8; 32]);
        let give = Pubkey::new_from_array([3u8; 32]);
        let want = Pubkey::new_from_array([4u8; 32]);
        let nonce = [5u8; 32];
        let b = encode_post_created(KIND_LISTING_PNFT, &maker, &post, &give, &want, 1, 5_000_000, 1_783_000_000, &nonce);

        assert_eq!(b.len(), 198);
        assert_eq!(&b[0..8], &[0x51, 0x55, 0xd0, 0xc6, 0x8d, 0xfd, 0x67, 0xfd]); // discriminator
        assert_eq!(&b[8..12], b"xete"); // brand magic
        assert_eq!(b[12], 1); // version
        assert_eq!(b[13], KIND_LISTING_PNFT);
        assert_eq!(&b[14..46], maker.as_ref());
        assert_eq!(&b[46..78], post.as_ref());
        assert_eq!(&b[78..110], give.as_ref());
        assert_eq!(&b[110..142], want.as_ref());
        assert_eq!(u64::from_le_bytes(b[142..150].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(b[150..158].try_into().unwrap()), 5_000_000);
        assert_eq!(i64::from_le_bytes(b[158..166].try_into().unwrap()), 1_783_000_000);
        assert_eq!(&b[166..198], &nonce);
    }

    #[test]
    fn discriminator_matches_anchor_convention() {
        // sha256("event:XetePostCreated")[..8] — recompute if you ever rename the event.
        use solana_program::hash::hashv;
        let h = hashv(&[b"event:XetePostCreated"]);
        assert_eq!(&DISC_POST_CREATED, &h.to_bytes()[..8]);
    }
}
