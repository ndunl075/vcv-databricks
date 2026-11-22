//! Byte scanning and hashing helpers.

use std::ptr;

pub const DAY_MS: i64 = 86_400_000;
pub const WEEK_MS: i64 = 7 * DAY_MS;
pub const GAP_MS: i64 = 1_800_000;

#[inline(always)]
pub fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

#[inline(always)]
pub fn rd64(b: &[u8], i: usize) -> u64 {
    debug_assert!(i + 8 <= b.len());
    unsafe { ptr::read_unaligned(b.as_ptr().add(i) as *const u64) }
}
#[inline(always)]
pub fn rd32(b: &[u8], i: usize) -> u32 {
    debug_assert!(i + 4 <= b.len());
    unsafe { ptr::read_unaligned(b.as_ptr().add(i) as *const u32) }
}

const LO7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
const ONES: u64 = 0x0101_0101_0101_0101;

/// High bit of every byte of `w` equal to `c` is set (exact, no false positives).
#[inline(always)]
pub fn eqmask(w: u64, c: u8) -> u64 {
    let x = w ^ (ONES.wrapping_mul(c as u64));
    !(((x & LO7).wrapping_add(LO7)) | x | LO7)
}

/// First index >= `from` of byte `c` in `h`.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn find_byte(h: &[u8], from: usize, c: u8) -> Option<usize> {
    use std::arch::x86_64::*;
    let n = h.len();
    let mut i = from;
    unsafe {
        let v = _mm_set1_epi8(c as i8);
        while i + 16 <= n {
            let x = _mm_loadu_si128(h.as_ptr().add(i) as *const __m128i);
            let m = _mm_movemask_epi8(_mm_cmpeq_epi8(x, v)) as u32;
            if m != 0 {
                return Some(i + m.trailing_zeros() as usize);
            }
            i += 16;
        }
    }
    while i < n {
        if h[i] == c {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub fn find_byte(h: &[u8], from: usize, c: u8) -> Option<usize> {
    let n = h.len();
    let mut i = from;
    while i + 8 <= n {
        let m = eqmask(rd64(h, i), c);
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < n {
        if h[i] == c {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// First index >= `from` of byte `c1` or `c2`.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn find2(h: &[u8], from: usize, c1: u8, c2: u8) -> Option<usize> {
    use std::arch::x86_64::*;
    let n = h.len();
    let mut i = from;
    unsafe {
        let v1 = _mm_set1_epi8(c1 as i8);
        let v2 = _mm_set1_epi8(c2 as i8);
        while i + 16 <= n {
            let x = _mm_loadu_si128(h.as_ptr().add(i) as *const __m128i);
            let m = _mm_movemask_epi8(_mm_or_si128(_mm_cmpeq_epi8(x, v1), _mm_cmpeq_epi8(x, v2))) as u32;
            if m != 0 {
                return Some(i + m.trailing_zeros() as usize);
            }
            i += 16;
        }
    }
    while i < n {
        let b = h[i];
        if b == c1 || b == c2 {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub fn find2(h: &[u8], from: usize, c1: u8, c2: u8) -> Option<usize> {
    let n = h.len();
    let mut i = from;
    while i + 8 <= n {
        let w = rd64(h, i);
        let m = eqmask(w, c1) | eqmask(w, c2);
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < n {
        let b = h[i];
        if b == c1 || b == c2 {
            return Some(i);
        }
        i += 1;
    }
    None
}

const K0: u64 = 0xa076_1d64_78bd_642f;
const K1: u64 = 0xe703_7ed1_a0b4_28db;
const K2: u64 = 0x8ebc_6af0_9c88_c6e3;
const K3: u64 = 0x5899_65cc_7537_4cc3;
const K4: u64 = 0x1d8e_4e27_c47d_124f;

#[inline(always)]
fn mum(a: u64, b: u64) -> u64 {
    let r = (a as u128).wrapping_mul(b as u128);
    (r as u64) ^ ((r >> 64) as u64)
}

/// 128-bit hash of a byte string, two independently mixed 64-bit halves.
#[inline]
pub fn hash128(b: &[u8]) -> (u64, u64) {
    let n = b.len();
    let (x, y);
    let mut s0 = K0;
    let mut s1 = K3;
    if n <= 16 {
        if n >= 8 {
            x = rd64(b, 0);
            y = rd64(b, n - 8);
        } else if n >= 4 {
            x = rd32(b, 0) as u64;
            y = rd32(b, n - 4) as u64;
        } else if n > 0 {
            x = ((b[0] as u64) << 16) | ((b[n >> 1] as u64) << 8) | b[n - 1] as u64;
            y = 0;
        } else {
            x = 0;
            y = 0;
        }
    } else {
        let mut i = 0;
        while n - i > 16 {
            let a = rd64(b, i);
            let c = rd64(b, i + 8);
            s0 = mum(a ^ K1, c ^ s0);
            s1 = mum(a ^ K2 ^ s1, c ^ K4);
            i += 16;
        }
        x = rd64(b, n - 16);
        y = rd64(b, n - 8);
    }
    let nn = n as u64;
    let lo = mum(x ^ K1 ^ nn, y ^ s0);
    let lo = mum(lo ^ K4, K2 ^ nn);
    let hi = mum(x ^ K2 ^ s1, y ^ K3 ^ nn.rotate_left(32));
    let hi = mum(hi ^ K0, K1);
    (lo, hi)
}

/// Byte string starting at `p` and ending at the first `,`, `"` or `\n`.
/// Valid for the content of an ID field that is followed by a delimiter in its line.
///
/// # Safety
/// `p` must point into a live mapping at the start of a field content that is
/// terminated by one of those bytes before the mapping ends.
#[inline]
pub unsafe fn field_at(p: u64) -> &'static [u8] {
    let s = p as usize as *const u8;
    let mut n = 0usize;
    loop {
        let b = *s.add(n);
        if b == b',' || b == b'"' || b == b'\n' {
            break;
        }
        n += 1;
    }
    std::slice::from_raw_parts(s, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bytes() {
        let h = b"abc,def\"ghijklmnopqrstuvwxyz,\n";
        assert_eq!(find_byte(h, 0, b','), Some(3));
        assert_eq!(find_byte(h, 4, b','), Some(28));
        assert_eq!(find_byte(h, 0, b'\n'), Some(29));
        assert_eq!(find_byte(h, 0, b'#'), None);
        assert_eq!(find2(h, 0, b',', b'"'), Some(3));
        assert_eq!(find2(h, 4, b',', b'"'), Some(7));
        assert_eq!(find2(h, 8, b',', b'"'), Some(28));
        for i in 0..h.len() {
            let want = h[i..].iter().position(|&c| c == b'\n').map(|p| p + i);
            assert_eq!(find_byte(h, i, b'\n'), want);
        }
    }

    #[test]
    fn swar_exact() {
        let w = u64::from_le_bytes(*b"a,b\"c,\x80\xff");
        let m = eqmask(w, b',');
        assert_eq!(m.trailing_zeros() >> 3, 1);
        assert_eq!(m.count_ones(), 2);
    }

    #[test]
    fn hashes_differ() {
        let a = hash128(b"u1");
        let b = hash128(b"u2");
        let c = hash128(b"U1");
        assert!(a != b && a != c && b != c);
        let l1 = hash128(b"0123456789abcdefghijklmnopqrstuvwxyz0");
        let l2 = hash128(b"0123456789abcdefghijklmnopqrstuvwxyz1");
        assert!(l1 != l2);
        assert!(hash128(b"") != hash128(b"\0"));
    }
}
