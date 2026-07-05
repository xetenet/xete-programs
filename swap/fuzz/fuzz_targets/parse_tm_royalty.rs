#![no_main]
use libfuzzer_sys::fuzz_target;
use xete_swap::fuzz_parse_tm_royalty;
// Untrusted Token-Metadata bytes -> royalty parser. Property: never panics / never OOB, for ANY input.
fuzz_target!(|data: &[u8]| { fuzz_parse_tm_royalty(data); });
