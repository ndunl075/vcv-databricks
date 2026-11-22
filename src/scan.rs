//! One pass over CSV bytes: find every comma, quote and line end 64 bytes at a time, then hand
//! each row with the right number of commas to a consumer, together with its quote count.
//!
//! The dialect (rules.md): only `\n` ends a row; every comma separates fields; a quote is legal
//! only as the first and last byte of a field. So a row is well formed exactly when it has the
//! right number of commas and its quote count is twice the number of fields that both start and
//! end with a quote (and are at least two bytes long). `Field::strip` does that bookkeeping.

/// Bit `i` of each mask is set when byte `i` of the block is that character.
pub struct Marks {
    pub comma: u64,
    pub line: u64,
    pub quote: u64,
}

/// A way to classify 64 bytes.
pub trait Lens {
    /// # Safety
    /// 64 bytes at `p` must be readable and the CPU must support the implementation.
    unsafe fn marks(p: *const u8) -> Marks;
}

/// Plain 64-bit arithmetic; works everywhere.
pub struct Words;

impl Lens for Words {
    #[inline(always)]
    unsafe fn marks(p: *const u8) -> Marks {
        const ONES: u64 = 0x0101_0101_0101_0101;
        const LOW7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
        #[inline(always)]
        fn hits(w: u64, c: u8) -> u64 {
            let x = w ^ (ONES * c as u64);
            let found = !(((x & LOW7) + LOW7) | x | LOW7);
            // One bit per byte, gathered into the low eight bits.
            ((found >> 7).wrapping_mul(0x0102_0408_1020_4080)) >> 56
        }
        let mut m = Marks { comma: 0, line: 0, quote: 0 };
        for i in 0..8 {
            let w = crate::ids::word(p.add(i * 8));
            m.comma |= hits(w, b',') << (i * 8);
            m.line |= hits(w, b'\n') << (i * 8);
            m.quote |= hits(w, b'"') << (i * 8);
        }
        m
    }
}

#[cfg(target_arch = "aarch64")]
pub struct Neon;

#[cfg(target_arch = "aarch64")]
impl Lens for Neon {
    #[inline(always)]
    unsafe fn marks(p: *const u8) -> Marks {
        use std::arch::aarch64::*;
        let v = [vld1q_u8(p), vld1q_u8(p.add(16)), vld1q_u8(p.add(32)), vld1q_u8(p.add(48))];
        #[inline(always)]
        unsafe fn gather(v: &[uint8x16_t; 4], c: u8) -> u64 {
            let want = vdupq_n_u8(c);
            let bit = vld1q_u8([1u8, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128].as_ptr());
            let a = vandq_u8(vceqq_u8(v[0], want), bit);
            let b = vandq_u8(vceqq_u8(v[1], want), bit);
            let c2 = vandq_u8(vceqq_u8(v[2], want), bit);
            let d = vandq_u8(vceqq_u8(v[3], want), bit);
            let s = vpaddq_u8(vpaddq_u8(a, b), vpaddq_u8(c2, d));
            let s = vpaddq_u8(s, s);
            vgetq_lane_u64::<0>(vreinterpretq_u64_u8(s))
        }
        Marks { comma: gather(&v, b','), line: gather(&v, b'\n'), quote: gather(&v, b'"') }
    }
}

#[cfg(target_arch = "x86_64")]
pub struct Sse;

#[cfg(target_arch = "x86_64")]
impl Lens for Sse {
    #[inline(always)]
    unsafe fn marks(p: *const u8) -> Marks {
        use std::arch::x86_64::*;
        let v = [
            _mm_loadu_si128(p as *const __m128i),
            _mm_loadu_si128(p.add(16) as *const __m128i),
            _mm_loadu_si128(p.add(32) as *const __m128i),
            _mm_loadu_si128(p.add(48) as *const __m128i),
        ];
        #[inline(always)]
        unsafe fn gather(v: &[__m128i; 4], c: u8) -> u64 {
            let want = _mm_set1_epi8(c as i8);
            let mut out = 0u64;
            for (i, x) in v.iter().enumerate() {
                out |= (_mm_movemask_epi8(_mm_cmpeq_epi8(*x, want)) as u32 as u64) << (16 * i);
            }
            out
        }
        Marks { comma: gather(&v, b','), line: gather(&v, b'\n'), quote: gather(&v, b'"') }
    }
}

#[cfg(target_arch = "x86_64")]
pub struct Avx;

#[cfg(target_arch = "x86_64")]
impl Lens for Avx {
    #[inline(always)]
    unsafe fn marks(p: *const u8) -> Marks {
        use std::arch::x86_64::*;
        let lo = _mm256_loadu_si256(p as *const __m256i);
        let hi = _mm256_loadu_si256(p.add(32) as *const __m256i);
        macro_rules! gather {
            ($c:expr) => {{
                let want = _mm256_set1_epi8($c as i8);
                let a = _mm256_movemask_epi8(_mm256_cmpeq_epi8(lo, want)) as u32 as u64;
                let b = _mm256_movemask_epi8(_mm256_cmpeq_epi8(hi, want)) as u32 as u64;
                a | (b << 32)
            }};
        }
        Marks { comma: gather!(b','), line: gather!(b'\n'), quote: gather!(b'"') }
    }
}

/// The lens every build can use without checking the CPU.
#[cfg(target_arch = "aarch64")]
pub type Plain = Neon;
#[cfg(target_arch = "x86_64")]
pub type Plain = Sse;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub type Plain = Words;

/// Bytes indexed per step.
pub const WINDOW: usize = 8192;
const BLOCKS: usize = WINDOW / 64;

/// Scratch space for one thread's scanning.
pub struct Index {
    commas: Vec<u32>,
    ends: Vec<u32>,
    quote_bits: [u64; BLOCKS + 1],
    quotes_before: [u32; BLOCKS + 1],
}

impl Index {
    pub fn new() -> Index {
        Index { commas: vec![0; WINDOW + 96], ends: vec![0; WINDOW + 96], quote_bits: [0; BLOCKS + 1], quotes_before: [0; BLOCKS + 1] }
    }
    /// Quotes in the current window before window offset `x`.
    #[inline(always)]
    fn quotes_to(&self, x: usize) -> u32 {
        let b = x >> 6;
        // SAFETY: x <= WINDOW, so b <= BLOCKS.
        unsafe { *self.quotes_before.get_unchecked(b) + (*self.quote_bits.get_unchecked(b) & ((1u64 << (x & 63)) - 1)).count_ones() }
    }
}

/// Receives rows that have exactly `N` commas.
pub trait Rows<const N: usize> {
    /// Called before each window is read; at most `rows` rows arrive before the next call.
    ///
    /// # Safety
    /// Only `rows_in` calls this.
    unsafe fn window(&mut self, rows: usize);
    /// One row: it starts at `from`, its commas are at `commas`, it ends before `to`, and it holds
    /// `quotes` quote characters. All offsets are from the start of the data.
    ///
    /// # Safety
    /// Only `rows_in` calls this.
    unsafe fn row(&mut self, from: usize, commas: &[usize; N], to: usize, quotes: u32);
}

/// Writes the offsets of the set bits of `m` (plus `base`) at `out[*n..]`.
#[inline(always)]
unsafe fn offsets(mut m: u64, base: u32, out: *mut u32, n: &mut usize, eager: usize) {
    let count = m.count_ones() as usize;
    let mut at = out.add(*n);
    // Writing a fixed number first keeps the common case free of data-dependent branches.
    for _ in 0..eager {
        // A volatile store keeps these as plain scalar stores; packing them into one vector
        // store costs more than it saves.
        at.write_volatile(base + m.trailing_zeros());
        m &= m.wrapping_sub(1);
        at = at.add(1);
    }
    while m != 0 {
        *at = base + m.trailing_zeros();
        m &= m.wrapping_sub(1);
        at = at.add(1);
    }
    *n += count;
}

/// Feeds every row of `data[from..to]` that has exactly `N` commas to `sink`.
/// `from` must be a row start and `to` a row start or the end of the data; `open_end` says the
/// data ends at `to` without a final `\n`, so the last row ends there.
///
/// # Safety
/// `data + to + 64` must be readable, and `L` must be usable on this CPU.
#[inline(always)]
pub unsafe fn rows_in<L: Lens, R: Rows<N>, const N: usize>(ix: &mut Index, data: *const u8, from: usize, to: usize, open_end: bool, sink: &mut R) {
    let commas = ix.commas.as_mut_ptr();
    let ends = ix.ends.as_mut_ptr();
    let mut row_at = from;
    // Commas and quotes of the current row that were seen in earlier windows.
    let mut held = [0usize; 8];
    let mut held_n = 0usize;
    let mut held_quotes = 0u32;
    let mut w = from;
    while w < to {
        let stop = (w + WINDOW).min(to);
        sink.window(WINDOW / (N + 1) + 2);
        let (mut nc, mut ne, mut q) = (0usize, 0usize, 0u32);
        let blocks = (stop - w + 63) / 64;
        for b in 0..blocks {
            let off = b * 64;
            let mut m = L::marks(data.add(w + off));
            let left = stop - w - off;
            if left < 64 {
                let keep = (1u64 << left) - 1;
                m.comma &= keep;
                m.line &= keep;
                m.quote &= keep;
            }
            *ix.quote_bits.get_unchecked_mut(b) = m.quote;
            *ix.quotes_before.get_unchecked_mut(b) = q;
            q += m.quote.count_ones();
            offsets(m.comma, off as u32, commas, &mut nc, 8);
            offsets(m.line, off as u32, ends, &mut ne, 2);
        }
        *ix.quote_bits.get_unchecked_mut(blocks) = 0;
        *ix.quotes_before.get_unchecked_mut(blocks) = q;
        if stop == to && open_end {
            *ends.add(ne) = (to - w) as u32;
            ne += 1;
        }
        for k in 0..=N {
            *commas.add(nc + k) = u32::MAX;
        }
        let mut ci = 0usize;
        let mut quotes_at_row = 0u32;
        for k in 0..ne {
            let end = *ends.add(k) as usize;
            let quotes_at_end = ix.quotes_to(end);
            let quotes = quotes_at_end - quotes_at_row + held_quotes;
            let mut c = [0usize; N];
            if row_at >= w {
                let fits = N == 0 || (*commas.add(ci + N - 1) as usize) < end;
                if fits && (*commas.add(ci + N) as usize) > end {
                    for (i, slot) in c.iter_mut().enumerate() {
                        *slot = w + *commas.add(ci + i) as usize;
                    }
                    ci += N;
                    sink.row(row_at, &c, w + end, quotes);
                } else {
                    while (*commas.add(ci) as usize) < end {
                        ci += 1;
                    }
                }
            } else {
                // The row began in an earlier window.
                let mut n = held_n;
                c[..n.min(N)].copy_from_slice(&held[..n.min(N)]);
                while (*commas.add(ci) as usize) < end {
                    if n < N {
                        c[n] = w + *commas.add(ci) as usize;
                    }
                    n += 1;
                    ci += 1;
                }
                if n == N {
                    sink.row(row_at, &c, w + end, quotes);
                }
            }
            row_at = w + end + 1;
            quotes_at_row = quotes_at_end;
            held_n = 0;
            held_quotes = 0;
        }
        while ci < nc {
            if held_n < 8 {
                held[held_n] = w + *commas.add(ci) as usize;
            }
            held_n = (held_n + 1).min(8);
            ci += 1;
        }
        held_quotes += q - quotes_at_row;
        w = stop;
    }
}

/// One field of a row during quote stripping.
pub struct Fields {
    pub quotes: u32,
    pub broken: usize,
}

impl Fields {
    #[inline(always)]
    pub fn new() -> Fields {
        Fields { quotes: 0, broken: 0 }
    }
    /// Removes the surrounding quotes of the field `data[*a..*e]`, if it has them, and notes
    /// anything that makes the row malformed.
    ///
    /// # Safety
    /// `data + *a` and `data + *e - 1` must be readable.
    #[inline(always)]
    pub unsafe fn strip(&mut self, data: *const u8, a: &mut usize, e: &mut usize) {
        let q = (*data.add(*a) == b'"') as usize;
        *a += q;
        *e -= q;
        self.broken |= q & (*data.add(*e) != b'"') as usize;
        self.broken |= (*e < *a) as usize;
        self.quotes += 2 * q as u32;
    }
    /// True when the row is well formed, given the number of quotes it holds.
    #[inline(always)]
    pub fn sound(&self, quotes_in_row: u32) -> bool {
        (self.broken == 0) & (self.quotes == quotes_in_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv::{for_each_line, split};

    struct Collect<const N: usize> {
        data: *const u8,
        got: Vec<Vec<Vec<u8>>>,
    }
    impl<const N: usize> Rows<N> for Collect<N> {
        unsafe fn window(&mut self, _rows: usize) {}
        unsafe fn row(&mut self, from: usize, commas: &[usize; N], to: usize, quotes: u32) {
            let mut f = Fields::new();
            let mut out = Vec::new();
            let mut a = from;
            for k in 0..=N {
                let mut e = if k < N { commas[k] } else { to };
                let mut s = a;
                f.strip(self.data, &mut s, &mut e);
                out.push((s, e));
                a = if k < N { commas[k] + 1 } else { to };
            }
            if f.sound(quotes) {
                self.got.push(out.iter().map(|&(s, e)| std::slice::from_raw_parts(self.data.add(s), e - s).to_vec()).collect());
            }
        }
    }

    fn reference<const N: usize>(data: &[u8], n: usize) -> Vec<Vec<Vec<u8>>> {
        let mut want = Vec::new();
        for_each_line(data, 0, n, |l| {
            let mut f = [(0usize, 0usize); 8];
            let ok = match N {
                1 => split::<2>(l, (&mut f[..2]).try_into().unwrap()),
                3 => split::<4>(l, (&mut f[..4]).try_into().unwrap()),
                _ => split::<7>(l, (&mut f[..7]).try_into().unwrap()),
            };
            if ok {
                want.push(f[..N + 1].iter().map(|&(a, b)| l[a..b].to_vec()).collect());
            }
        });
        want
    }

    fn check<L: Lens, const N: usize>(text: &[u8]) {
        let mut data = text.to_vec();
        let n = data.len();
        data.extend_from_slice(&[b'#'; 200]);
        let mut ix = Index::new();
        let mut sink = Collect::<N> { data: data.as_ptr(), got: Vec::new() };
        let open = n > 0 && data[n - 1] != b'\n';
        unsafe { rows_in::<L, _, N>(&mut ix, data.as_ptr(), 0, n, open, &mut sink) };
        assert_eq!(sink.got, reference::<N>(&data, n), "{:?}", String::from_utf8_lossy(text));
    }

    fn every_lens<const N: usize>(text: &[u8]) {
        check::<Words, N>(text);
        check::<Plain, N>(text);
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            #[target_feature(enable = "avx2")]
            unsafe fn go<const N: usize>(text: &[u8]) {
                check::<Avx, N>(text);
            }
            unsafe { go::<N>(text) };
        }
    }

    #[test]
    fn random_text_matches_the_reference_splitter() {
        let alphabet = *b"ab,\",,\"\n\n1\r ";
        let mut seed = 0x243f_6a88_85a3_08d3u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..4000 {
            let len = (next() % 300) as usize;
            let mut data: Vec<u8> = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            if round % 3 == 0 {
                data.push(b'\n');
            }
            every_lens::<1>(&data);
            every_lens::<3>(&data);
            every_lens::<6>(&data);
        }
    }

    #[test]
    fn rows_longer_than_a_window() {
        let mut seed = 0x1357_9bdf_0246_8aceu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..40 {
            let mut data = Vec::new();
            while data.len() < 5 * WINDOW {
                let kind = next() % 8;
                let field = |n: u64, quoted: bool| {
                    let mut v = Vec::new();
                    if quoted {
                        v.push(b'"');
                    }
                    v.extend(std::iter::repeat(b'x').take(n as usize));
                    if quoted {
                        v.push(b'"');
                    }
                    v
                };
                let long = if kind == 0 { WINDOW as u64 + next() % 9000 } else { next() % 40 };
                data.extend(field(long, next() % 2 == 0));
                data.push(b',');
                data.extend(field(next() % 30, next() % 3 == 0));
                if kind == 1 {
                    data.push(b'"');
                }
                if kind == 2 {
                    data.extend_from_slice(b",extra");
                }
                data.push(b'\n');
            }
            if round % 2 == 0 {
                data.pop();
            }
            every_lens::<1>(&data);
        }
    }

    #[test]
    fn window_edges() {
        for pad in [WINDOW - 70, WINDOW - 8, WINDOW - 4, WINDOW - 3, WINDOW - 2, WINDOW - 1, WINDOW, WINDOW + 1, 2 * WINDOW - 1, 2 * WINDOW] {
            for tail in ["a,b\n", "\"a\",\"b\"\n", "a,b", "\"a\",b", "a,\"b\"", ",\n", ",", "\n", ""] {
                let mut data = Vec::new();
                while data.len() + 6 < pad {
                    data.extend_from_slice(b"\"q\",z\n");
                }
                while data.len() < pad {
                    data.push(b'y');
                }
                data.extend_from_slice(tail.as_bytes());
                every_lens::<1>(&data);
            }
        }
    }
}
