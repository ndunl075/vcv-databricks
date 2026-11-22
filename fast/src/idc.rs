//! The generator's ID codec (kit/generator/generator/src/text.rs, user.rs) and key recovery.
//!
//! An ID is one letter plus lowercase base 36 (no leading zeros) of `scramble(number, key)`,
//! a bijection on 40 bits: multiply by A, xor the key, xor-shift right 17, multiply by B.
//! A, B are fixed; the three keys (users, events, clicks) change per seed.
//! Exactness never depends on a key: `decode` is a bijection on canonical IDs, so a wrong key
//! only moves IDs outside the capacities (checked) and the run hands off.

use std::sync::atomic::{AtomicU64, Ordering};

pub const MASK: u64 = (1 << 40) - 1;
const A: u64 = 0x9E37_79B9_7F4B;
const B: u64 = 0xD6E8_FEB8_6659;
const fn inv(x: u64) -> u64 {
    let mut y = x;
    let mut i = 0;
    while i < 6 {
        y = y.wrapping_mul(2u64.wrapping_sub(x.wrapping_mul(y)));
        i += 1;
    }
    y & MASK
}
const AINV: u64 = inv(A);
const BINV: u64 = inv(B);

#[inline(always)]
pub fn scramble(v: u64, key: u64) -> u64 {
    let mut m = (v & MASK).wrapping_mul(A);
    m = (m ^ key) & MASK;
    m ^= m >> 17;
    m.wrapping_mul(B) & MASK
}

/// The key-independent half of the inverse.
#[inline(always)]
pub fn unmix(code: u64) -> u64 {
    let x = code.wrapping_mul(BINV) & MASK;
    x ^ (x >> 17) ^ (x >> 34)
}

#[inline(always)]
pub fn decode(u: u64, key: u64) -> u64 {
    ((u ^ key) & MASK).wrapping_mul(AINV) & MASK
}

/// Number (before scrambling) of a canonical code.
#[inline(always)]
pub fn number(code: u64, key: u64) -> u64 {
    decode(unmix(code), key)
}

/// The code of an ID `prefix` + canonical base 36: lowercase, 1 to 8 digits, no leading zero
/// (except the single digit `0`), value below 2^40. Anything else is not a generator ID.
#[inline]
pub fn code_of(b: &[u8], prefix: u8) -> Option<u64> {
    let n = b.len();
    if !(2..=9).contains(&n) || b[0] != prefix {
        return None;
    }
    if b[1] == b'0' && n > 2 {
        return None;
    }
    let mut v = 0u64;
    for &c in &b[1..] {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'z' => c - b'a' + 10,
            _ => return None,
        };
        v = v * 36 + d as u64;
    }
    if v > MASK {
        None
    } else {
        Some(v)
    }
}

/// Writes `prefix` + base 36 of `code` into `out`; returns the length.
pub fn write_id(out: &mut [u8; 10], prefix: u8, code: u64) -> usize {
    const D: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut tmp = [0u8; 9];
    let mut at = tmp.len();
    let mut r = code;
    loop {
        at -= 1;
        tmp[at] = D[(r % 36) as usize];
        r /= 36;
        if r == 0 {
            break;
        }
    }
    out[0] = prefix;
    let n = tmp.len() - at;
    out[1..1 + n].copy_from_slice(&tmp[at..]);
    1 + n
}

const POW36: [u64; 9] = [1, 36, 1296, 46_656, 1_679_616, 60_466_176, 2_176_782_336, 78_364_164_096, 2_821_109_907_456];

/// Sort key that orders canonical IDs of one prefix byte-wise: digits left-aligned (a missing
/// digit sorts before every digit), then the length.
#[inline]
pub fn lex_key(code: u64) -> u64 {
    let mut len = 1;
    while len < 8 && code >= POW36[len] {
        len += 1;
    }
    ((code * POW36[8 - len]) << 4) | len as u64
}

/// Finds the key of a counter dense in `0..2^bits` from distinct sample codes.
pub fn find_dense_key(codes: &[u64], bits: u32, threads: usize) -> Option<u64> {
    if codes.len() < 12 {
        return None;
    }
    let us: Vec<u64> = codes.iter().take(24).map(|&c| unmix(c)).collect();
    let u0 = us[0];
    let total = 1u64 << bits;
    let found = AtomicU64::new(u64::MAX);
    let next = AtomicU64::new(0);
    let step = (total / (threads as u64 * 16)).clamp(1 << 12, 1 << 22);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                if found.load(Ordering::Relaxed) != u64::MAX {
                    return;
                }
                let lo = next.fetch_add(step, Ordering::Relaxed);
                if lo >= total {
                    return;
                }
                let hi = (lo + step).min(total);
                let mut w = lo.wrapping_mul(A) & MASK;
                for _ in lo..hi {
                    let k = u0 ^ w;
                    w = (w + A) & MASK;
                    if (((us[1] ^ k).wrapping_mul(AINV) & MASK) >> bits) != 0 {
                        continue;
                    }
                    if us[2..].iter().all(|&u| (((u ^ k).wrapping_mul(AINV) & MASK) >> bits) == 0) {
                        found.store(k, Ordering::Relaxed);
                        return;
                    }
                }
            });
        }
    });
    let k = found.load(Ordering::Relaxed);
    (k != u64::MAX).then_some(k)
}

/// Finds the user key. Numbers are `user * 512 + alias`, aliases mostly below 16.
/// Step 1: the low 9 key bits, by which guess makes most sampled aliases small.
/// Step 2: enumerate the user number of one sample and verify on the others.
pub fn find_user_key(codes: &[u64], ubits: u32) -> Option<u64> {
    if codes.len() < 12 {
        return None;
    }
    let us: Vec<u64> = codes.iter().map(|&c| unmix(c)).collect();
    let mut best = (0usize, 0u64);
    let mut second = 0usize;
    for k9 in 0..512u64 {
        // The first alias is used by about half of all rows, so the true low bits make alias 0
        // the mode; a wrong guess shifts it to some other small alias (hence not `< 16`).
        let c = us.iter().filter(|&&u| ((u & 511) ^ k9) == 0).count();
        if c > best.0 {
            second = best.0;
            best = (c, k9);
        } else if c > second {
            second = c;
        }
    }
    let k9 = best.1;
    if std::env::var_os("SSF_LOG").is_some() {
        eprintln!("user key step 1: n={} best={:?} second={}", us.len(), best, second);
    }
    if best.0 * 10 < us.len() * 3 || second * 2 > best.0 {
        return None;
    }
    // Distinct samples for the verification, one with a small alias as the anchor.
    let mut distinct: Vec<u64> = us.clone();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < 12 {
        return None;
    }
    let anchor = *distinct.iter().find(|&&u| (((u & 511) ^ k9).wrapping_mul(AINV) & 511) < 16)?;
    let alias0 = ((anchor & 511) ^ k9).wrapping_mul(AINV) & 511;
    let others: Vec<u64> = distinct.iter().copied().filter(|&u| u != anchor).take(24).collect();
    let total = 1u64 << ubits;
    for u0 in 0..total {
        let w = ((u0 << 9) | alias0).wrapping_mul(A) & MASK;
        let k = anchor ^ w;
        if (((others[0] ^ k).wrapping_mul(AINV) & MASK) >> (9 + ubits)) != 0 {
            continue;
        }
        if others.iter().all(|&u| (((u ^ k).wrapping_mul(AINV) & MASK) >> (9 + ubits)) == 0) {
            return Some(k);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_inverse() {
        let key = 0x1234_5678_9abc & MASK;
        for v in [0u64, 1, 2, 511, 512, 123_456_789, (1 << 39) + 3, MASK] {
            let c = scramble(v, key);
            assert_eq!(number(c, key), v);
        }
        assert_eq!(AINV.wrapping_mul(A) & MASK, 1);
        assert_eq!(BINV.wrapping_mul(B) & MASK, 1);
    }

    #[test]
    fn code_forms() {
        assert_eq!(code_of(b"e0", b'e'), Some(0));
        assert_eq!(code_of(b"e00", b'e'), None);
        assert_eq!(code_of(b"e01", b'e'), None);
        assert_eq!(code_of(b"E1", b'e'), None);
        assert_eq!(code_of(b"e1A", b'e'), None);
        assert_eq!(code_of(b"e", b'e'), None);
        assert_eq!(code_of(b"e123456789", b'e'), None);
        assert_eq!(code_of(b"ezzzzzzzz", b'e'), None); // above 2^40
        let mut buf = [0u8; 10];
        for c in [0u64, 35, 36, 1295, 1296, 123_456_789, MASK] {
            let n = write_id(&mut buf, b'u', c);
            assert_eq!(code_of(&buf[..n], b'u'), Some(c));
        }
    }

    #[test]
    fn lex_order_matches_bytes() {
        let mut buf = [0u8; 10];
        let mut codes: Vec<u64> = vec![0, 1, 35, 36, 37, 1295, 1296, 46_656, 46_657, 100_000, 5_000_000_000, MASK, 1 << 30];
        codes.sort_by_key(|&c| lex_key(c));
        let strs: Vec<Vec<u8>> = codes
            .iter()
            .map(|&c| {
                let n = write_id(&mut buf, b'u', c);
                buf[..n].to_vec()
            })
            .collect();
        let mut sorted = strs.clone();
        sorted.sort();
        assert_eq!(strs, sorted);
    }

    #[test]
    fn keys_are_found() {
        let key = 0x0a1b_2c3d_4e5f & MASK;
        let codes: Vec<u64> = (0..40u64).map(|i| scramble(i * 7919 % 5000, key)).collect();
        assert_eq!(find_dense_key(&codes, 14, 2), Some(key));
        let ukey = 0x7766_5544_3322 & MASK;
        let ucodes: Vec<u64> = (0..300u64).map(|i| scramble((i * 31 % 900) * 512 + (i % 3), ukey)).collect();
        assert_eq!(find_user_key(&ucodes, 12), Some(ukey));
    }
}
