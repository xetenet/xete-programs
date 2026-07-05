//! Royalty parsing + dust-safe split. Custody-grade: FAIL-CLOSED, every read bounds-checked, the
//! borsh variable-length strings WALKED (never fixed-offset). The chain (asset metadata) is the sole
//! source of truth for WHERE royalties go; this module extracts (bps, creators+shares), the settle
//! path enforces payment per the negotiated mode. Proven in isolation by the tests below before wiring.

use solana_program::{program_error::ProgramError, pubkey::Pubkey};

/// Token-Metadata's creator limit. Assets with more share-holding creators are unsupported (revert).
pub(crate) const MAX_CREATORS: usize = 5;

/// Parse `seller_fee_basis_points` + creators[(address, share)] from a Token-Metadata `Metadata`
/// account's raw data. Walks the borsh strings (name/symbol/uri = u32-LE len + bytes) to reach the
/// fields — works whether the strings are puffed-to-max or exact. Any overrun / bad tag / shape ⇒ Err.
/// Returns (bps, creators, count). The caller must already have verified the acct is the canonical
/// metadata PDA for the mint AND owned by the Token-Metadata program (no spoofed metadata).
pub(crate) fn parse_tm_royalty(d: &[u8]) -> Result<(u16, [(Pubkey, u8); MAX_CREATORS], usize), ProgramError> {
    let bad = || ProgramError::InvalidAccountData;
    // key(1) + update_authority(32) + mint(32) = 65
    if d.len() < 65 {
        return Err(bad());
    }
    let mut o = 65usize;
    // walk 3 borsh strings: name, symbol, uri
    for _ in 0..3 {
        if o + 4 > d.len() {
            return Err(bad());
        }
        let len = u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize;
        o = o.checked_add(4).ok_or_else(bad)?.checked_add(len).ok_or_else(bad)?;
        if o > d.len() {
            return Err(bad());
        }
    }
    // seller_fee_basis_points u16
    if o + 2 > d.len() {
        return Err(bad());
    }
    let bps = u16::from_le_bytes([d[o], d[o + 1]]);
    if bps > 10_000 {
        return Err(bad());
    }
    o += 2;
    // creators Option<Vec<Creator>> : opt(1) [+ vec_len u32 + Creator{addr32,verified1,share1} * n]
    if o >= d.len() {
        return Err(bad());
    }
    let opt = d[o];
    o += 1;
    let mut creators = [(Pubkey::new_from_array([0u8; 32]), 0u8); MAX_CREATORS];
    let mut n = 0usize;
    if opt == 1 {
        if o + 4 > d.len() {
            return Err(bad());
        }
        let vlen = u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize;
        o += 4;
        if vlen > MAX_CREATORS {
            return Err(bad());
        }
        let end = o.checked_add(vlen.checked_mul(34).ok_or_else(bad)?).ok_or_else(bad)?;
        if end > d.len() {
            return Err(bad());
        }
        for slot in creators.iter_mut().take(vlen) {
            let mut a = [0u8; 32];
            a.copy_from_slice(&d[o..o + 32]);
            match d[o + 32] {
                0 | 1 => {} // `verified` must be a real bool
                _ => return Err(bad()),
            }
            *slot = (Pubkey::new_from_array(a), d[o + 33]); // (creator, share)
            o += 34;
            n += 1;
        }
    } else if opt != 0 {
        return Err(bad()); // Option flag must be 0 or 1
    }
    Ok((bps, creators, n))
}

/// Dust-safe split of `total` across `shares` (which MUST sum to 100): floor each, the LAST gets the
/// remainder so the payouts sum to `total` EXACTLY — no leakage either way. `shares` = shares[0..n].
pub(crate) fn split_by_share(total: u64, shares: &[u8]) -> [u64; MAX_CREATORS] {
    let mut out = [0u64; MAX_CREATORS];
    let n = shares.len();
    if n == 0 {
        return out;
    }
    let mut paid = 0u64;
    for (i, &sh) in shares.iter().enumerate().take(n - 1) {
        let amt = ((total as u128 * sh as u128) / 100u128) as u64;
        out[i] = amt;
        paid += amt;
    }
    out[n - 1] = total - paid; // remainder to last; total >= paid because Σshare==100 and each floored
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Token-Metadata account body: key + update_auth + mint + name/symbol/uri (borsh) + bps + creators.
    fn meta(name: &str, symbol: &str, uri: &str, bps: u16, creators: Option<&[([u8; 32], u8, u8)]>) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(4u8); // Key::MetadataV1
        v.extend_from_slice(&[0u8; 32]); // update_authority
        v.extend_from_slice(&[0u8; 32]); // mint
        for s in [name, symbol, uri] {
            v.extend_from_slice(&(s.len() as u32).to_le_bytes());
            v.extend_from_slice(s.as_bytes());
        }
        v.extend_from_slice(&bps.to_le_bytes());
        match creators {
            None => v.push(0),
            Some(cs) => {
                v.push(1);
                v.extend_from_slice(&(cs.len() as u32).to_le_bytes());
                for (a, ver, sh) in cs {
                    v.extend_from_slice(a);
                    v.push(*ver);
                    v.push(*sh);
                }
            }
        }
        v
    }

    #[test]
    fn single_creator() {
        let a = [7u8; 32];
        let d = meta("Cool NFT", "COOL", "https://x/y.json", 500, Some(&[(a, 1, 100)]));
        let (bps, cr, n) = parse_tm_royalty(&d).unwrap();
        assert_eq!(bps, 500);
        assert_eq!(n, 1);
        assert_eq!(cr[0], (Pubkey::new_from_array(a), 100));
    }

    #[test]
    fn multi_creator_and_split() {
        let d = meta("N", "S", "U", 750, Some(&[([1; 32], 1, 60), ([2; 32], 1, 30), ([3; 32], 0, 10)]));
        let (bps, cr, n) = parse_tm_royalty(&d).unwrap();
        assert_eq!((bps, n), (750, 3));
        let shares = [cr[0].1, cr[1].1, cr[2].1];
        let out = split_by_share(1000, &shares[..n]);
        assert_eq!(&out[..n], &[600, 300, 100]);
        assert_eq!(out[..n].iter().sum::<u64>(), 1000);
    }

    #[test]
    fn dust_goes_to_last() {
        let out = split_by_share(7, &[50, 50]);
        assert_eq!(&out[..2], &[3, 4]); // floor(3.5)=3, remainder 4
        assert_eq!(out[..2].iter().sum::<u64>(), 7);
        let out2 = split_by_share(10, &[33, 33, 34]);
        assert_eq!(out2[..3].iter().sum::<u64>(), 10);
    }

    #[test]
    fn none_creators() {
        let d = meta("N", "S", "U", 250, None);
        let (bps, _c, n) = parse_tm_royalty(&d).unwrap();
        assert_eq!((bps, n), (250, 0));
    }

    #[test]
    fn walks_variable_length_strings() {
        // non-puffed short name + symbol + a near-max uri — only correct if we follow the borsh lengths
        let d = meta("A long collection name goes here", "SYM", &"x".repeat(199), 333, Some(&[([5; 32], 1, 100)]));
        let (bps, cr, n) = parse_tm_royalty(&d).unwrap();
        assert_eq!((bps, n, cr[0].1), (333, 1, 100));
    }

    #[test]
    fn rejects_truncated() {
        let d = meta("N", "S", "U", 250, Some(&[([1; 32], 1, 100)]));
        assert!(parse_tm_royalty(&d[..d.len() - 5]).is_err()); // chop into creators
        assert!(parse_tm_royalty(&d[..60]).is_err()); // chop the header
        assert!(parse_tm_royalty(&[]).is_err());
    }

    #[test]
    fn rejects_bad_option_byte() {
        let mut d = meta("N", "S", "U", 250, None);
        *d.last_mut().unwrap() = 2; // Option flag = 2 (invalid)
        assert!(parse_tm_royalty(&d).is_err());
    }

    #[test]
    fn rejects_bad_verified_byte() {
        let d = meta("N", "S", "U", 250, Some(&[([1; 32], 9, 100)])); // verified = 9
        assert!(parse_tm_royalty(&d).is_err());
    }

    #[test]
    fn rejects_too_many_creators() {
        let six: Vec<([u8; 32], u8, u8)> =
            (0..6u8).map(|i| ([i; 32], 1, if i == 0 { 50 } else { 10 })).collect();
        let d = meta("N", "S", "U", 250, Some(&six));
        assert!(parse_tm_royalty(&d).is_err()); // vlen 6 > MAX_CREATORS
    }

    #[test]
    fn rejects_bps_over_max() {
        let d = meta("N", "S", "U", 10_001, None);
        assert!(parse_tm_royalty(&d).is_err());
    }
}

#[cfg(test)]
mod split_invariants {
    use super::{split_by_share, MAX_CREATORS};
    // For EVERY valid partition (n in 1..=5, shares sum to 100) the caller (settle.rs) can pass:
    // split_by_share never panics AND payouts sum to EXACTLY total (dust-safe, zero leakage).
    #[test]
    fn splits_sum_to_total_and_never_panic() {
        let totals = [0u64, 1, 7, 100, 101, 999, 1_000_000, u64::MAX / 2];
        let partitions: &[&[u8]] = &[
            &[100], &[50, 50], &[1, 99], &[99, 1], &[33, 33, 34], &[25, 25, 25, 25],
            &[20, 20, 20, 20, 20], &[96, 1, 1, 1, 1], &[1, 1, 1, 1, 96], &[60, 40], &[10, 20, 30, 40],
        ];
        for &t in &totals {
            for p in partitions {
                assert_eq!(p.iter().map(|&x| x as u32).sum::<u32>(), 100);
                assert!(p.len() <= MAX_CREATORS);
                let out = split_by_share(t, p);
                let s: u128 = out[..p.len()].iter().map(|&x| x as u128).sum();
                assert_eq!(s, t as u128, "payouts must sum to total exactly (t={t}, p={p:?})");
            }
        }
    }
}

/// Fuzz-only pub shim so the external fuzz crate can reach the crate-private parser WITHOUT changing the
/// production build. Exists only under `--cfg fuzzing` (which only cargo-fuzz sets) — absent otherwise.
#[cfg(fuzzing)]
pub fn fuzz_parse_tm_royalty(d: &[u8]) {
    let _ = parse_tm_royalty(d);
}
