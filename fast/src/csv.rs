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
}
