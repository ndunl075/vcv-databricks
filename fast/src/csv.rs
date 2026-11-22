//! The CSV dialect of rules.md: split a line into fields.
//!
//! No value ever holds a comma, a quote or a newline, so every comma separates fields.
//! A field is either quote-free, or wrapped in exactly one pair of quotes; any other quote
//! makes the line malformed. A `\r` is an ordinary byte.

use crate::util::{find2, find_byte};

/// Splits `line` into exactly `N` fields. `out[i]` is the (start, end) of field `i`'s value
/// (quotes stripped). Returns false when the line is malformed or has another field count.
#[inline]
pub fn split<const N: usize>(line: &[u8], out: &mut [(usize, usize); N]) -> bool {
    let len = line.len();
    let mut nf = 0usize;
    let mut p = 0usize;
    loop {
        if nf == N {
            return false;
        }
        if p < len && line[p] == b'"' {
            // Quoted field: the next delimiter must be the closing quote.
            match find2(line, p + 1, b',', b'"') {
                Some(q) if line[q] == b'"' => {
                    out[nf] = (p + 1, q);
                    nf += 1;
                    let after = q + 1;
                    if after == len {
                        return nf == N;
                    }
                    if line[after] != b',' {
                        return false;
                    }
                    p = after + 1;
                }
                _ => return false,
            }
        } else {
            match find2(line, p, b',', b'"') {
                None => {
                    out[nf] = (p, len);
                    nf += 1;
                    return nf == N;
                }
                Some(q) => {
                    if line[q] != b',' {
                        return false; // a quote in the middle of a field
                    }
                    out[nf] = (p, q);
                    nf += 1;
                    p = q + 1;
                }
            }
        }
    }
}

/// Calls `f` for every line (without its `\n`) in `data[start..end]`. `start` must be a line start
/// and `end` a line start or the end of the data.
#[inline]
pub fn for_each_line<'a>(data: &'a [u8], start: usize, end: usize, mut f: impl FnMut(&'a [u8])) {
    let hay = &data[..end];
    let mut p = start;
    while p < end {
        let e = find_byte(hay, p, b'\n').unwrap_or(end);
        f(&data[p..e]);
        p = e + 1;
    }
}

/// Newline, comma and quote bitmasks of `data[i..i + 16]` (needs `i + 16 <= data.len()`).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn masks16(data: &[u8], i: usize) -> (u32, u32, u32) {
    use std::arch::x86_64::*;
    unsafe {
        let x = _mm_loadu_si128(data.as_ptr().add(i) as *const __m128i);
        let nl = _mm_movemask_epi8(_mm_cmpeq_epi8(x, _mm_set1_epi8(b'\n' as i8))) as u32;
        let cm = _mm_movemask_epi8(_mm_cmpeq_epi8(x, _mm_set1_epi8(b',' as i8))) as u32;
        let q = _mm_movemask_epi8(_mm_cmpeq_epi8(x, _mm_set1_epi8(b'"' as i8))) as u32;
        (nl, cm, q)
    }
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn masks16(data: &[u8], i: usize) -> (u32, u32, u32) {
    let (mut nl, mut cm, mut q) = (0u32, 0u32, 0u32);
    for k in 0..16 {
        let b = data[i + k];
        nl |= ((b == b'\n') as u32) << k;
        cm |= ((b == b',') as u32) << k;
        q |= ((b == b'"') as u32) << k;
    }
    (nl, cm, q)
}

/// Fast path of `line`: delimiter bitmasks, no per-field scans. `None` for anything unusual
/// (wrong field count, odd quotes, line too near the end of the data): the caller then
/// runs the exact scalar path, so the result is always that of `split`.
#[inline(always)]
fn fastsplit<const N: usize>(data: &[u8], p: usize, out: &mut [(usize, usize); N]) -> Option<usize> {
    let mut nf = 0usize;
    let mut fstart = p;
    let mut quotes = 0u32;
    let mut i = p;
    loop {
        if i + 16 > data.len() {
            return None;
        }
        let (nl, cm, q) = masks16(data, i);
        let mut ev = nl | cm;
        while ev != 0 {
            let t = ev.trailing_zeros();
            ev &= ev - 1;
            let pos = i + t as usize;
            if nf == N {
                return None;
            }
            out[nf] = (fstart, pos);
            nf += 1;
            fstart = pos + 1;
            if (nl >> t) & 1 != 0 {
                if nf != N {
                    return None;
                }
                quotes += (q & ((1u32 << t) - 1)).count_ones();
                // Quoted fields: first and last byte, nothing else quoted.
                let mut quoted = 0u32;
                for f in out.iter_mut() {
                    let (a, b) = *f;
                    if b > a && data[a] == b'"' {
                        if b - a < 2 || data[b - 1] != b'"' {
                            return None;
                        }
                        *f = (a + 1, b - 1);
                        quoted += 1;
                    }
                }
                return if quotes == 2 * quoted { Some(pos + 1) } else { None };
            }
        }
        quotes += q.count_ones();
        i += 16;
    }
}

/// Calls `f` with the fields of every well-formed `N`-field line in `data[s..e]`.
#[inline(always)]
pub fn for_each_row<const N: usize>(data: &[u8], s: usize, e: usize, mut f: impl FnMut(&[(usize, usize); N])) {
    let mut p = s;
    let mut out = [(0usize, 0usize); N];
    while p < e {
        let (next, ok) = line::<N>(data, p, e, &mut out);
        p = next;
        if ok {
            f(&out);
        }
    }
}

/// Splits the line starting at `p` (the chunk ends at `e`, a line start) into exactly `N`
/// fields, as absolute `(start, end)` offsets of the unquoted values. Returns the start of the
/// next line and whether the line is well-formed.
#[inline(always)]
pub fn line<const N: usize>(data: &[u8], p: usize, e: usize, out: &mut [(usize, usize); N]) -> (usize, bool) {
    if let Some(next) = fastsplit::<N>(data, p, out) {
        return (next, true);
    }
    let le = find_byte(&data[..e], p, b'\n').unwrap_or(e);
    let mut tmp = [(0usize, 0usize); N];
    let ok = split::<N>(&data[p..le], &mut tmp);
    if ok {
        for k in 0..N {
            out[k] = (tmp[k].0 + p, tmp[k].1 + p);
        }
    }
    (le + 1, ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp<const N: usize>(s: &str) -> Option<Vec<String>> {
        let mut o = [(0, 0); N];
        if split::<N>(s.as_bytes(), &mut o) {
            Some(o.iter().map(|&(a, b)| s[a..b].to_string()).collect())
        } else {
            None
        }
    }

    #[test]
    fn basics() {
        assert_eq!(sp::<3>("a,b,c").unwrap(), ["a", "b", "c"]);
        assert_eq!(sp::<3>("\"a\",b,\"c\"").unwrap(), ["a", "b", "c"]);
        assert_eq!(sp::<3>("\"\",,\"\"").unwrap(), ["", "", ""]);
        assert_eq!(sp::<3>(",,").unwrap(), ["", "", ""]);
        assert_eq!(sp::<2>(" u1,\" u1\"").unwrap(), [" u1", " u1"]);
        assert_eq!(sp::<2>("a,b\r").unwrap(), ["a", "b\r"]);
    }

    #[test]
    fn malformed() {
        assert!(sp::<3>("a,b").is_none());
        assert!(sp::<3>("a,b,c,d").is_none());
        assert!(sp::<3>("").is_none());
        assert!(sp::<1>("").is_some());
        assert!(sp::<2>("a,b\"").is_none());
        assert!(sp::<2>("a,\"b").is_none());
        assert!(sp::<2>("a\"b,c").is_none());
        assert!(sp::<2>("\"a,b\"").is_none());
        assert!(sp::<2>("\"a\"x,b").is_none());
        assert!(sp::<2>("x\"a\",b").is_none());
        assert!(sp::<2>("\"a\"\",b").is_none());
        assert!(sp::<2>("\"\"\",b").is_none());
        assert!(sp::<2>("\"a\r,b").is_none());
        assert!(sp::<2>("\"a\"\r,b").is_none());
        assert!(sp::<2>("a,\"b\"\r").is_none());
        assert!(sp::<2>("\",b").is_none());
        assert!(sp::<2>("a,\"").is_none());
    }

    #[test]
    fn line_matches_split() {
        // Random lines over a small alphabet: `line` must agree with `split` on every one.
        let alpha = [b'a', b'b', b',', b',', b'"', b'\n', b'\r', b' ', b'1'];
        let mut x = 0x1234_5678_9abc_def1u64;
        for round in 0..200_000 {
            let mut data = Vec::new();
            let len = (round % 90) + 1;
            for _ in 0..len {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                data.push(alpha[(x % alpha.len() as u64) as usize]);
            }
            // Pad so that 16-byte blocks fit, with a newline-terminated last line.
            data.extend_from_slice(b"\nzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n");
            let e = data.len();
            for &n in &[2usize, 4, 7] {
                let mut p = 0;
                while p < e {
                    let le = find_byte(&data[..e], p, b'\n').unwrap_or(e);
                    let mut want = [(0usize, 0usize); 7];
                    let ok_want = match n {
                        2 => { let mut t = [(0, 0); 2]; let o = split::<2>(&data[p..le], &mut t); for k in 0..2 { want[k] = (t[k].0 + p, t[k].1 + p); } o }
                        4 => { let mut t = [(0, 0); 4]; let o = split::<4>(&data[p..le], &mut t); for k in 0..4 { want[k] = (t[k].0 + p, t[k].1 + p); } o }
                        _ => { let mut t = [(0, 0); 7]; let o = split::<7>(&data[p..le], &mut t); for k in 0..7 { want[k] = (t[k].0 + p, t[k].1 + p); } o }
                    };
                    let mut got = [(0usize, 0usize); 7];
                    let (next, ok) = match n {
                        2 => { let mut t = [(0, 0); 2]; let r = line::<2>(&data, p, e, &mut t); got[..2].copy_from_slice(&t); r }
                        4 => { let mut t = [(0, 0); 4]; let r = line::<4>(&data, p, e, &mut t); got[..4].copy_from_slice(&t); r }
                        _ => { let mut t = [(0, 0); 7]; let r = line::<7>(&data, p, e, &mut t); got.copy_from_slice(&t); r }
                    };
                    assert_eq!(next, le + 1);
                    assert_eq!(ok, ok_want, "line {:?} n {n}", String::from_utf8_lossy(&data[p..le]));
                    if ok {
                        assert_eq!(&got[..n], &want[..n]);
                    }
                    p = le + 1;
                }
            }
        }
    }
}
