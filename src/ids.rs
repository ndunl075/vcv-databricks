//! Exact identities for ID strings.
//!
//! An ID of up to 23 bytes is its own key: the bytes, zero padded, plus the length in the last
//! byte, so two keys are equal exactly when the strings are. A longer ID is keyed by a 128-bit
//! hash and a marker, and every hit on such a key is confirmed by comparing the bytes.
//!
//! `Names` hands out one small number per distinct user ID, from every thread at once. All later
//! stages work on those numbers.

use crate::os;
use crate::util::hash128;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::RwLock;

pub type Key = [u64; 3];

/// Longest ID stored inside its key.
pub const INLINE: usize = 23;
const LONG_TAG: u64 = 0xff << 56;

const fn word_masks() -> [[u64; 3]; INLINE + 1] {
    let mut t = [[0u64; 3]; INLINE + 1];
    let mut n = 0;
    while n <= INLINE {
        let mut w = 0;
        while w < 3 {
            let have = if n > w * 8 { n - w * 8 } else { 0 };
            t[n][w] = if have >= 8 { !0 } else { (1u64 << (have * 8)) - 1 };
            w += 1;
        }
        n += 1;
    }
    t
}
static WORD_MASKS: [[u64; 3]; INLINE + 1] = word_masks();

/// Masks keeping the low `n` bytes of a little-endian word, `n` in 0..=8.
pub static LOW_BYTES: [u64; 9] = [0, 0xff, 0xffff, 0xff_ffff, 0xffff_ffff, 0xff_ffff_ffff, 0xffff_ffff_ffff, 0xff_ffff_ffff_ffff, !0];

/// Unaligned little-endian word at `p`.
///
/// # Safety
/// Eight bytes at `p` must be readable.
#[inline(always)]
pub unsafe fn word(p: *const u8) -> u64 {
    u64::from_le(std::ptr::read_unaligned(p as *const u64))
}

/// The key of the `n` bytes at `p`.
///
/// # Safety
/// `p..p+n` must be readable, and so must the 24 bytes at `p` (inputs carry a zero tail).
#[inline(always)]
pub unsafe fn key_of(p: *const u8, n: usize) -> Key {
    if n <= INLINE {
        let m = &WORD_MASKS[n];
        [word(p) & m[0], word(p.add(8)) & m[1], (word(p.add(16)) & m[2]) | ((n as u64) << 56)]
    } else {
        long_key(p, n)
    }
}

#[cold]
#[inline(never)]
unsafe fn long_key(p: *const u8, n: usize) -> Key {
    let (a, b) = hash128(std::slice::from_raw_parts(p, n));
    [a, b, LONG_TAG | (n as u64 & 0x00ff_ffff_ffff_ffff)]
}

#[inline(always)]
pub fn is_long(k: &Key) -> bool {
    k[2] >> 56 == 0xff
}

#[inline(always)]
fn fold(a: u64, b: u64) -> u64 {
    let r = (a as u128).wrapping_mul(b as u128);
    (r as u64) ^ ((r >> 64) as u64)
}

/// 64 well-mixed bits of a key.
#[inline(always)]
pub fn spread(k: &Key) -> u64 {
    let x = fold(k[0] ^ 0x9e37_79b9_7f4a_7c15, k[1] ^ 0xc2b2_ae3d_27d4_eb4f);
    fold(x ^ k[2] ^ 0x1656_67b1_9e37_79f9, 0xd6e8_feb8_6659_fd93)
}

/// Longest ID that is its own key in the wide table.
pub const WIDE: usize = 55;

/// Key of an ID of 24 to 55 bytes: the bytes, zero padded, with the length in the last byte.
pub type WideKey = [u64; 7];

/// # Safety
/// `n <= WIDE`, and the 56 bytes at `p` must be readable (inputs carry a zero tail).
#[inline(always)]
pub unsafe fn wide_key_of(p: *const u8, n: usize) -> WideKey {
    let mut k = [0u64; 7];
    for (w, slot) in k.iter_mut().enumerate() {
        let have = n.saturating_sub(w * 8).min(8);
        *slot = word(p.add(w * 8)) & *LOW_BYTES.get_unchecked(have);
    }
    k[6] |= (n as u64) << 56;
    k
}

/// 64 well-mixed bits of a wide key.
#[inline(always)]
pub fn spread_wide(k: &WideKey) -> u64 {
    let a = fold(k[0] ^ 0x9e37_79b9_7f4a_7c15, k[1] ^ 0xc2b2_ae3d_27d4_eb4f);
    let b = fold(k[2] ^ 0x1656_67b1_9e37_79f9, k[3] ^ 0x27d4_eb2f_1656_67c5);
    let c = fold(k[4] ^ 0x8ebc_6af0_9c88_c6e3, k[5] ^ 0x5899_65cc_7537_4cc3);
    fold(a ^ b.rotate_left(23) ^ c.rotate_left(41) ^ k[6], 0xd6e8_feb8_6659_fd93)
}

/// A hash of the `n` bytes at `p`: equal strings give equal hashes.
///
/// # Safety
/// As `key_of` and `wide_key_of`.
#[inline(always)]
pub unsafe fn hash_of(p: *const u8, n: usize) -> u64 {
    if n <= INLINE {
        spread(&key_of(p, n))
    } else if n <= WIDE {
        spread_wide(&wide_key_of(p, n))
    } else {
        spread(&long_key(p, n))
    }
}

/// An ID's text: its first 16 bytes (zero padded) kept here, and where all of it lives.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Text {
    pub at: *const u8,
    pub len: u32,
    pub first: [u8; 16],
}

impl Text {
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        if self.len <= 16 {
            &self.first[..self.len as usize]
        } else {
            // SAFETY: `at..at+len` lies in an input mapping that is never unmapped.
            unsafe { std::slice::from_raw_parts(self.at, self.len as usize) }
        }
    }
    /// Byte-wise order of two IDs.
    #[inline]
    pub fn before(&self, other: &Text) -> bool {
        let (a, b) = (u128::from_be_bytes(self.first), u128::from_be_bytes(other.first));
        if a != b {
            a < b
        } else {
            // SAFETY: as in `bytes`.
            unsafe { std::slice::from_raw_parts(self.at, self.len as usize) < std::slice::from_raw_parts(other.at, other.len as usize) }
        }
    }
}

#[repr(C, align(32))]
struct Cell {
    key: Key,
    /// 0: free, 1: being written, otherwise the ID's number plus 2.
    state: AtomicU64,
}

#[repr(C, align(64))]
struct WideCell {
    key: WideKey,
    /// As `Cell::state`.
    state: AtomicU64,
}

const NUMBERS_PER_GRAB: u32 = 1024;
const PAGE_BITS: u32 = 16;
const PAGES: usize = 1 << (32 - PAGE_BITS);

pub struct Names {
    cells: AtomicPtr<Cell>,
    bits: AtomicU32,
    /// Readers hold this shared while they use `cells`; growing takes it exclusively.
    pub gate: RwLock<()>,
    used: AtomicUsize,
    /// The same again for IDs of 24 to 55 bytes, which need wider cells.
    wide_cells: AtomicPtr<WideCell>,
    wide_bits: AtomicU32,
    wide_used: AtomicUsize,
    pub grow_wanted: AtomicBool,
    next: AtomicU32,
    pages: Box<[AtomicPtr<Text>]>,
}

// SAFETY: all shared state is atomics or memory handed out to one writer at a time.
unsafe impl Sync for Names {}
unsafe impl Send for Names {}

/// A snapshot of the table, valid while the gate is held shared.
#[derive(Clone, Copy)]
pub struct Table {
    cells: *mut Cell,
    shift: u32,
    mask: usize,
    full_at: usize,
    wide_cells: *mut WideCell,
    wide_shift: u32,
    wide_mask: usize,
    wide_full_at: usize,
}

/// One thread's supply of unused numbers.
pub struct Supply {
    next: u32,
    end: u32,
    pub fresh: usize,
    fresh_wide: usize,
}

impl Supply {
    pub fn new() -> Supply {
        Supply { next: 0, end: 0, fresh: 0, fresh_wide: 0 }
    }
}

impl Names {
    pub fn new(expected: usize, threads: usize) -> Names {
        let want = (expected * 2).max(1 << 18).max(threads << 16).next_power_of_two();
        let bits = want.trailing_zeros();
        let wide_bits = bits.saturating_sub(3).max(16);
        Names {
            cells: AtomicPtr::new(os::zeroed_array::<Cell>(want)),
            bits: AtomicU32::new(bits),
            gate: RwLock::new(()),
            used: AtomicUsize::new(0),
            wide_cells: AtomicPtr::new(os::zeroed_array::<WideCell>(1 << wide_bits)),
            wide_bits: AtomicU32::new(wide_bits),
            wide_used: AtomicUsize::new(0),
            grow_wanted: AtomicBool::new(false),
            next: AtomicU32::new(0),
            pages: (0..PAGES).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(),
        }
    }

    /// Call with the gate held shared.
    pub fn table(&self) -> Table {
        let bits = self.bits.load(Acquire);
        let wb = self.wide_bits.load(Acquire);
        Table {
            cells: self.cells.load(Acquire),
            shift: 64 - bits,
            mask: (1usize << bits) - 1,
            full_at: 1usize << (bits - 1),
            wide_cells: self.wide_cells.load(Acquire),
            wide_shift: 64 - wb,
            wide_mask: (1usize << wb) - 1,
            wide_full_at: 1usize << (wb - 1),
        }
    }

    /// One more than the largest number handed out so far.
    pub fn count(&self) -> usize {
        self.next.load(SeqCst) as usize
    }

    #[inline(always)]
    pub fn text(&self, id: u32) -> *mut Text {
        let page = self.pages[(id >> PAGE_BITS) as usize].load(Relaxed);
        // SAFETY: the page was installed before `id` was handed out.
        unsafe { page.add((id & ((1 << PAGE_BITS) - 1)) as usize) }
    }

    #[cold]
    #[inline(never)]
    fn refill(&self, s: &mut Supply) {
        let first = self.next.fetch_add(NUMBERS_PER_GRAB, SeqCst);
        if first > u32::MAX - 2 * NUMBERS_PER_GRAB {
            eprintln!("error: more than four billion distinct user IDs");
            std::process::exit(1);
        }
        let slot = &self.pages[(first >> PAGE_BITS) as usize];
        if slot.load(Acquire).is_null() {
            let fresh = os::zeroed_array::<Text>(1 << PAGE_BITS);
            // Losing the race leaves one unused region behind; it is never touched.
            let _ = slot.compare_exchange(std::ptr::null_mut(), fresh, AcqRel, Acquire);
        }
        s.next = first;
        s.end = first + NUMBERS_PER_GRAB;
    }

    /// Adds this thread's new IDs to the shared totals; true when a table should grow.
    #[inline]
    pub fn report(&self, s: &mut Supply, t: &Table) -> bool {
        let total = if s.fresh > 0 {
            let n = self.used.fetch_add(s.fresh, Relaxed) + s.fresh;
            s.fresh = 0;
            n
        } else {
            self.used.load(Relaxed)
        };
        let wide = if s.fresh_wide > 0 {
            let n = self.wide_used.fetch_add(s.fresh_wide, Relaxed) + s.fresh_wide;
            s.fresh_wide = 0;
            n
        } else {
            self.wide_used.load(Relaxed)
        };
        (total > t.full_at) | (wide > t.wide_full_at) | self.grow_wanted.load(Relaxed)
    }

    /// Grows whichever table is more than half full until it is at most a quarter full.
    /// Call without holding the gate.
    #[cold]
    pub fn grow(&self) {
        self.grow_wanted.store(true, SeqCst);
        let _all = self.gate.write().unwrap();
        // SAFETY: exclusive access; every cell is free or complete.
        unsafe {
            let bits = self.bits.load(Acquire);
            let used = self.used.load(SeqCst);
            if used > 1usize << (bits - 1) {
                let nb = roomy(bits, used);
                let old = self.cells.load(Acquire);
                let new = os::zeroed_array::<Cell>(1usize << nb);
                let mask = (1usize << nb) - 1;
                for i in 0..1usize << bits {
                    let c = &*old.add(i);
                    let st = c.state.load(Relaxed);
                    if st >= 2 {
                        let mut j = (spread(&c.key) >> (64 - nb)) as usize;
                        while (*new.add(j)).state.load(Relaxed) != 0 {
                            j = (j + 1) & mask;
                        }
                        (*new.add(j)).key = c.key;
                        (*new.add(j)).state.store(st, Relaxed);
                    }
                }
                os::release(old as *const u8, (1usize << bits) * std::mem::size_of::<Cell>());
                self.cells.store(new, Release);
                self.bits.store(nb, Release);
            }
            let bits = self.wide_bits.load(Acquire);
            let used = self.wide_used.load(SeqCst);
            if used > 1usize << (bits - 1) {
                let nb = roomy(bits, used);
                let old = self.wide_cells.load(Acquire);
                let new = os::zeroed_array::<WideCell>(1usize << nb);
                let mask = (1usize << nb) - 1;
                for i in 0..1usize << bits {
                    let c = &*old.add(i);
                    let st = c.state.load(Relaxed);
                    if st >= 2 {
                        let mut j = (spread_wide(&c.key) >> (64 - nb)) as usize;
                        while (*new.add(j)).state.load(Relaxed) != 0 {
                            j = (j + 1) & mask;
                        }
                        (*new.add(j)).key = c.key;
                        (*new.add(j)).state.store(st, Relaxed);
                    }
                }
                os::release(old as *const u8, (1usize << bits) * std::mem::size_of::<WideCell>());
                self.wide_cells.store(new, Release);
                self.wide_bits.store(nb, Release);
            }
        }
        self.grow_wanted.store(false, SeqCst);
    }
}

/// Table size (as a power of two) that leaves `used` cells at most a quarter of it.
fn roomy(bits: u32, used: usize) -> u32 {
    let mut nb = bits + 1;
    while used > 1usize << (nb - 2) {
        nb += 1;
    }
    nb
}

impl Table {
    #[inline(always)]
    pub fn warm(&self, h: u64) {
        // SAFETY: the index is inside the table.
        os::soon(unsafe { self.cells.add((h >> self.shift) as usize) });
    }

    /// The number of the ID with key `k` (hash `h`, bytes `at..at+len`), giving it one if new.
    ///
    /// # Safety
    /// The gate must be held shared; `at..at+len` must be the bytes `k` was made from.
    #[inline(always)]
    pub unsafe fn number(&self, names: &Names, s: &mut Supply, k: &Key, h: u64, at: *const u8, len: usize) -> u32 {
        let mut i = (h >> self.shift) as usize;
        loop {
            let c = self.cells.add(i);
            let st = (*c).state.load(Acquire);
            if st >= 2 {
                let ck = &(*c).key;
                if (ck[0] == k[0]) & (ck[1] == k[1]) & (ck[2] == k[2]) {
                    let id = (st - 2) as u32;
                    if !is_long(k) || same_long(names, id, at, len) {
                        return id;
                    }
                }
                i = (i + 1) & self.mask;
            } else if st == 0 {
                if (*c).state.compare_exchange(0, 1, AcqRel, Acquire).is_ok() {
                    if s.next == s.end {
                        names.refill(s);
                    }
                    let id = s.next;
                    s.next += 1;
                    s.fresh += 1;
                    let key_at = std::ptr::addr_of_mut!((*c).key);
                    key_at.write(*k);
                    names.text(id).write(Text { at, len: len as u32, first: first_of(k, at) });
                    (*c).state.store(id as u64 + 2, Release);
                    return id;
                }
            } else {
                std::hint::spin_loop();
            }
        }
    }

    /// As `number`, for an ID of 24 to `WIDE` bytes; its key is rebuilt from the bytes.
    ///
    /// # Safety
    /// As `number`; `h` must be `spread_wide` of the ID's wide key.
    #[inline(always)]
    unsafe fn number_wide(&self, names: &Names, s: &mut Supply, h: u64, at: *const u8, len: usize) -> u32 {
        let k = wide_key_of(at, len);
        let mut i = (h >> self.wide_shift) as usize;
        loop {
            let c = self.wide_cells.add(i);
            let st = (*c).state.load(Acquire);
            if st >= 2 {
                if (*c).key == k {
                    return (st - 2) as u32;
                }
                i = (i + 1) & self.wide_mask;
            } else if st == 0 {
                if (*c).state.compare_exchange(0, 1, AcqRel, Acquire).is_ok() {
                    if s.next == s.end {
                        names.refill(s);
                    }
                    let id = s.next;
                    s.next += 1;
                    s.fresh_wide += 1;
                    std::ptr::addr_of_mut!((*c).key).write(k);
                    let mut first = [0u8; 16];
                    std::ptr::copy_nonoverlapping(at, first.as_mut_ptr(), 16);
                    names.text(id).write(Text { at, len: len as u32, first });
                    (*c).state.store(id as u64 + 2, Release);
                    return id;
                }
            } else {
                std::hint::spin_loop();
            }
        }
    }

    /// Starts the lookup of the ID `at..at+len`: its hash, its key when it has a narrow one, and
    /// a request for the cache line the lookup will read. `finish` completes it.
    ///
    /// # Safety
    /// As `key_of` and `wide_key_of`.
    #[inline(always)]
    pub unsafe fn begin(&self, at: *const u8, len: usize) -> (Key, u64) {
        if len <= INLINE {
            let k = key_of(at, len);
            let h = spread(&k);
            self.warm(h);
            (k, h)
        } else if len <= WIDE {
            let h = spread_wide(&wide_key_of(at, len));
            os::soon(self.wide_cells.add((h >> self.wide_shift) as usize));
            ([0; 3], h)
        } else {
            let k = long_key(at, len);
            let h = spread(&k);
            self.warm(h);
            (k, h)
        }
    }

    /// The ID's number, given what `begin` returned for the same bytes.
    ///
    /// # Safety
    /// As `number`.
    #[inline(always)]
    pub unsafe fn finish(&self, names: &Names, s: &mut Supply, k: &Key, h: u64, at: *const u8, len: usize) -> u32 {
        if (len <= INLINE) | (len > WIDE) {
            self.number(names, s, k, h, at, len)
        } else {
            self.number_wide(names, s, h, at, len)
        }
    }
}

#[cold]
#[inline(never)]
unsafe fn same_long(names: &Names, id: u32, at: *const u8, len: usize) -> bool {
    (*names.text(id)).bytes() == std::slice::from_raw_parts(at, len)
}

/// The first 16 bytes of an ID, zero padded.
#[inline(always)]
unsafe fn first_of(k: &Key, at: *const u8) -> [u8; 16] {
    let mut out = [0u8; 16];
    if is_long(k) {
        std::ptr::copy_nonoverlapping(at, out.as_mut_ptr(), 16);
    } else {
        out[..8].copy_from_slice(&k[0].to_le_bytes());
        out[8..].copy_from_slice(&k[1].to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn padded(s: &[u8]) -> Vec<u8> {
        let mut v = s.to_vec();
        v.extend_from_slice(&[0u8; 64]);
        v
    }

    #[test]
    fn keys_are_exact_for_short_ids() {
        let cases: Vec<&[u8]> = vec![b"a", b"a\0", b"a\0\0", b"ab", b"u1", b"U1", b"12345678", b"123456789", b"1234567890123456", b"12345678901234567890123", b"\0", b"\0\0"];
        for (i, a) in cases.iter().enumerate() {
            for (j, b) in cases.iter().enumerate() {
                let (pa, pb) = (padded(a), padded(b));
                let (ka, kb) = unsafe { (key_of(pa.as_ptr(), a.len()), key_of(pb.as_ptr(), b.len())) };
                assert_eq!(ka == kb, i == j, "{a:?} {b:?}");
            }
        }
        // Bytes after the ID never leak into the key.
        let mut noisy = b"abc".to_vec();
        noisy.extend_from_slice(&[0xee; 64]);
        assert_eq!(unsafe { key_of(noisy.as_ptr(), 3) }, unsafe { key_of(padded(b"abc").as_ptr(), 3) });
    }

    #[test]
    fn numbers_are_stable_and_distinct() {
        let names = Names::new(4, 1);
        let mut supply = Supply::new();
        let pool: Vec<Vec<u8>> = (0..5000).map(|i| padded(format!("user-{i}-{}", "x".repeat(i % 70)).as_bytes())).collect();
        let lens: Vec<usize> = pool.iter().map(|p| p.len() - 64).collect();
        let _g = names.gate.read().unwrap();
        let t = names.table();
        let mut got = Vec::new();
        for (p, &n) in pool.iter().zip(&lens) {
            let (k, h) = unsafe { t.begin(p.as_ptr(), n) };
            got.push(unsafe { t.finish(&names, &mut supply, &k, h, p.as_ptr(), n) });
        }
        let mut uniq = got.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), pool.len());
        for (i, (p, &n)) in pool.iter().zip(&lens).enumerate() {
            let (k, h) = unsafe { t.begin(p.as_ptr(), n) };
            assert_eq!(unsafe { t.finish(&names, &mut supply, &k, h, p.as_ptr(), n) }, got[i]);
            assert_eq!(unsafe { (*names.text(got[i])).bytes() }, &p[..n]);
        }
    }

    #[test]
    fn growing_keeps_every_number() {
        let names = Names::new(4, 1);
        let mut supply = Supply::new();
        let pool: Vec<Vec<u8>> = (0..400_000).map(|i| padded(if i % 3 == 0 { format!("g{i}-0123456789abcdef0123456789abcdef") } else { format!("g{i}") }.as_bytes())).collect();
        let mut got = Vec::new();
        for chunk in pool.chunks(1000) {
            {
                let _g = names.gate.read().unwrap();
                let t = names.table();
                for p in chunk {
                    let n = p.len() - 64;
                    let (k, h) = unsafe { t.begin(p.as_ptr(), n) };
                    got.push(unsafe { t.finish(&names, &mut supply, &k, h, p.as_ptr(), n) });
                }
                if !names.report(&mut supply, &t) {
                    continue;
                }
            }
            names.grow();
        }
        let _g = names.gate.read().unwrap();
        let t = names.table();
        assert!(t.mask + 1 > 1 << 18 && t.wide_mask + 1 > 1 << 16);
        for (i, p) in pool.iter().enumerate() {
            let n = p.len() - 64;
            let (k, h) = unsafe { t.begin(p.as_ptr(), n) };
            assert_eq!(unsafe { t.finish(&names, &mut supply, &k, h, p.as_ptr(), n) }, got[i]);
            assert_eq!(unsafe { (*names.text(got[i])).bytes() }, &p[..n]);
        }
        let mut uniq = got.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), pool.len());
    }

    #[test]
    fn text_orders_like_bytes() {
        let items: Vec<&[u8]> = vec![b"U9", b"u1", b"u10", b"u2", b"u", b"u\0", b"abcdefgh", b"abcdefgha", b"abcdefgh\0", b"0123456789abcdef", b"0123456789abcdefg", b"0123456789abcdef\0", b"0123456789abcdefghijklmnopqrstuvwxyz", b"0123456789abcdefghijklmnopqrstuvwxyy"];
        for a in &items {
            for b in &items {
                let (pa, pb) = (padded(a), padded(b));
                let (ka, kb) = unsafe { (key_of(pa.as_ptr(), a.len()), key_of(pb.as_ptr(), b.len())) };
                let ta = Text { at: pa.as_ptr(), len: a.len() as u32, first: unsafe { first_of(&ka, pa.as_ptr()) } };
                let tb = Text { at: pb.as_ptr(), len: b.len() as u32, first: unsafe { first_of(&kb, pb.as_ptr()) } };
                assert_eq!(ta.before(&tb), a < b, "{a:?} {b:?}");
                assert_eq!(ta.bytes(), *a);
            }
        }
    }
}
