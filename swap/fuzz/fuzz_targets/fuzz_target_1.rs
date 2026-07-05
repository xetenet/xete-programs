#![no_main]
use libfuzzer_sys::fuzz_target;
use solana_program::pubkey::Pubkey;
use xete_swap::ed25519_layout_ok;

// Fuzz the attacker-controlled ed25519 instruction data (the parse surface). Fixed signer + message;
// the property under test is: ed25519_layout_ok never panics / never reads out of bounds, for ANY input.
fuzz_target!(|data: &[u8]| {
    let signer = Pubkey::new_from_array([7u8; 32]);
    let msg = b"AXTSWAP:order:1|x";
    let _ = ed25519_layout_ok(data, &signer, msg);
});
