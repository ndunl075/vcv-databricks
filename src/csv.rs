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

// ---------------------------------------------------------------------------------------------
// One-pass row reader.

/// Bytes examined per step, and a bit mask of the delimiters (`,` `"` `\n`) among them.
#[cfg(target_arch = "x86_64")]
const STEP: usize = 16;
#[cfg(not(target_arch = "x86_64"))]
const STEP: usize = 8;

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn delimiters(hay: &[u8], at: usize) -> u32 {
    use std::arch::x86_64::*;
    debug_assert!(at + STEP <= hay.len());
    // SAFETY: the caller guarantees 16 readable bytes at `at`; SSE2 is part of the x86-64 baseline.
    unsafe {
        let x = _mm_loadu_si128(hay.as_ptr().add(at) as *const __m128i);
        let hit = _mm_or_si128(
            _mm_or_si128(_mm_cmpeq_epi8(x, _mm_set1_epi8(b',' as i8)), _mm_cmpeq_epi8(x, _mm_set1_epi8(b'"' as i8))),
            _mm_cmpeq_epi8(x, _mm_set1_epi8(b'\n' as i8)),
        );
        _mm_movemask_epi8(hit) as u32
    }
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn delimiters(hay: &[u8], at: usize) -> u32 {
    use crate::util::{eqmask, rd64};
    let w = rd64(hay, at);
    let m = eqmask(w, b',') | eqmask(w, b'"') | eqmask(w, b'\n');
    (((m >> 7).wrapping_mul(0x0102_0408_1020_4080)) >> 56) as u32
}

struct RowState<const N: usize> {
    fields: [(usize, usize); N],
    count: usize,
    line_start: usize,
    field_start: usize,
    /// 0: no quote seen in this field, 1: quote opened, 2: quote closed.
    quote: u8,
    quote_close: usize,
    bad: bool,
}

impl<const N: usize> RowState<N> {
    #[inline(always)]
    fn finish_field(&mut self, at: usize) {
        let (a, b) = match self.quote {
            0 => (self.field_start, at),
            1 => {
                self.bad = true;
                return;
            }
            _ => {
                if self.quote_close + 1 != at {
                    self.bad = true;
                    return;
                }
                (self.field_start + 1, self.quote_close)
            }
        };
        if self.count == N {
            self.bad = true;
            return;
        }
        self.fields[self.count] = (a - self.line_start, b - self.line_start);
        self.count += 1;
    }

    /// Handles the delimiter byte `c` found at `at`.
    #[inline(always)]
    fn delimiter<'a>(&mut self, data: &'a [u8], at: usize, c: u8, emit: &mut impl FnMut(&'a [u8], &[(usize, usize); N])) {
        match c {
            b',' => {
                if !self.bad {
                    self.finish_field(at);
                }
                self.field_start = at + 1;
                self.quote = 0;
            }
            b'"' => {
                if !self.bad {
                    match self.quote {
                        0 if at == self.field_start => self.quote = 1,
                        1 => {
                            self.quote = 2;
                            self.quote_close = at;
                        }
                        _ => self.bad = true,
                    }
                }
            }
            _ => self.end_row(data, at, emit),
        }
    }

    #[inline(always)]
    fn end_row<'a>(&mut self, data: &'a [u8], at: usize, emit: &mut impl FnMut(&'a [u8], &[(usize, usize); N])) {
        if !self.bad {
            self.finish_field(at);
            if !self.bad && self.count == N {
                emit(&data[self.line_start..at], &self.fields);
            }
        }
        self.line_start = at + 1;
        self.field_start = at + 1;
        self.count = 0;
        self.quote = 0;
        self.bad = false;
    }
}

/// Calls `emit(line, fields)` for every well-formed row with exactly `N` fields in
/// `data[start..end]`; `fields` are (start, end) offsets inside `line`, quotes stripped.
/// `start` must be a row start; `end` a row start or the end of the data.
pub fn for_each_row<'a, const N: usize>(data: &'a [u8], start: usize, end: usize, mut emit: impl FnMut(&'a [u8], &[(usize, usize); N])) {
    let hay = &data[..end];
    let mut st = RowState::<N> {
        fields: [(0, 0); N],
        count: 0,
        line_start: start,
        field_start: start,
        quote: 0,
        quote_close: 0,
        bad: false,
    };
    let mut at = start;
    while at + STEP <= end {
        let mut m = delimiters(hay, at);
        while m != 0 {
            let p = at + m.trailing_zeros() as usize;
            m &= m - 1;
            st.delimiter(data, p, hay[p], &mut emit);
        }
        at += STEP;
    }
    while at < end {
        let c = hay[at];
        if c == b',' || c == b'"' || c == b'\n' {
            st.delimiter(data, at, c, &mut emit);
        }
        at += 1;
    }
    if st.line_start < end {
        st.end_row(data, end, &mut emit);
    }
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
    fn row_reader_matches_split() {
        let alphabet = *b"ab,\",,\"\n\n1\r ";
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..3000 {
            let len = (next() % 90) as usize;
            let mut data: Vec<u8> = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            if round % 3 == 0 {
                data.push(b'\n');
            }
            for width in [2usize, 3] {
                let mut want: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
                let mut got: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
                for_each_line(&data, 0, data.len(), |l| {
                    if width == 2 {
                        let mut f = [(0, 0); 2];
                        if split::<2>(l, &mut f) {
                            want.push((l.to_vec(), f.iter().map(|&(a, b)| l[a..b].to_vec()).collect()));
                        }
                    } else {
                        let mut f = [(0, 0); 3];
                        if split::<3>(l, &mut f) {
                            want.push((l.to_vec(), f.iter().map(|&(a, b)| l[a..b].to_vec()).collect()));
                        }
                    }
                });
                if width == 2 {
                    for_each_row::<2>(&data, 0, data.len(), |l, f| got.push((l.to_vec(), f.iter().map(|&(a, b)| l[a..b].to_vec()).collect())));
                } else {
                    for_each_row::<3>(&data, 0, data.len(), |l, f| got.push((l.to_vec(), f.iter().map(|&(a, b)| l[a..b].to_vec()).collect())));
                }
                assert_eq!(got, want, "{:?} width {width}", String::from_utf8_lossy(&data));
            }
        }
    }
}
