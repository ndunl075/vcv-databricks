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

/// Where an ID's bytes live, and its first eight bytes in comparison order.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Text {
    pub head: u64,
    pub at: *const u8,
    pub len: u32,
}

impl Text {
    #[inline]
    pub fn bytes(&self) -> &'static [u8] {
        // SAFETY: `at..at+len` lies in an input mapping that is never unmapped.
        unsafe { std::slice::from_raw_parts(self.at, self.len as usize) }
    }
    /// Byte-wise order of two IDs.
    #[inline]
    pub fn before(&self, other: &Text) -> bool {
        if self.head != other.head {
            self.head < other.head
        } else {
            self.bytes() < other.bytes()
        }
    }
}

#[repr(C, align(32))]
struct Cell {
    key: Key,
    /// 0: free, 1: being written, otherwise the ID's number plus 2.
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
}

/// One thread's supply of unused numbers.
pub struct Supply {
    next: u32,
    end: u32,
    pub fresh: usize,
}

impl Supply {
    pub fn new() -> Supply {
        Supply { next: 0, end: 0, fresh: 0 }
    }
}

impl Names {
    pub fn new(expected: usize, threads: usize) -> Names {
        let want = (expected * 2).max(1 << 18).max(threads << 16).next_power_of_two();
        let bits = want.trailing_zeros();
        Names {
            cells: AtomicPtr::new(os::zeroed_array::<Cell>(want)),
            bits: AtomicU32::new(bits),
            gate: RwLock::new(()),
            used: AtomicUsize::new(0),
            grow_wanted: AtomicBool::new(false),
            next: AtomicU32::new(0),
            pages: (0..PAGES).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(),
        }
    }

    /// Call with the gate held shared.
    pub fn table(&self) -> Table {
        let bits = self.bits.load(Acquire);
        Table { cells: self.cells.load(Acquire), shift: 64 - bits, mask: (1usize << bits) - 1, full_at: 1usize << (bits - 1) }
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

    /// Adds this thread's new IDs to the shared total; true when the table should grow.
    #[inline]
    pub fn report(&self, s: &mut Supply, t: &Table) -> bool {
        let total = if s.fresh > 0 {
            let n = self.used.fetch_add(s.fresh, Relaxed) + s.fresh;
            s.fresh = 0;
            n
        } else {
            self.used.load(Relaxed)
        };
        total > t.full_at || self.grow_wanted.load(Relaxed)
    }

    /// Doubles the table until it is at most a quarter full. Call without holding the gate.
    #[cold]
    pub fn grow(&self) {
        self.grow_wanted.store(true, SeqCst);
        let _all = self.gate.write().unwrap();
        let bits = self.bits.load(Acquire);
        let used = self.used.load(SeqCst);
        if used > 1usize << (bits - 1) {
            let mut nb = bits + 1;
            while used > 1usize << (nb - 2) {
                nb += 1;
            }
            let old = self.cells.load(Acquire);
            let new = os::zeroed_array::<Cell>(1usize << nb);
            let mask = (1usize << nb) - 1;
            // SAFETY: exclusive access; every old cell is free or complete.
            unsafe {
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
            }
            self.cells.store(new, Release);
            self.bits.store(nb, Release);
        }
        self.grow_wanted.store(false, SeqCst);
    }
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
                    names.text(id).write(Text { head: head_of(at, len), at, len: len as u32 });
                    (*c).state.store(id as u64 + 2, Release);
                    return id;
                }
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

#[cold]
#[inline(never)]
unsafe fn same_long(names: &Names, id: u32, at: *const u8, len: usize) -> bool {
    (*names.text(id)).bytes() == std::slice::from_raw_parts(at, len)
}

/// First eight bytes, zero padded, as a number that orders like the bytes do.
#[inline(always)]
unsafe fn head_of(at: *const u8, len: usize) -> u64 {
    (word(at) & LOW_BYTES[len.min(8)]).swap_bytes()
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
        let pool: Vec<Vec<u8>> = (0..5000).map(|i| padded(format!("user-{i}-{}", "x".repeat(i % 40)).as_bytes())).collect();
        let lens: Vec<usize> = pool.iter().map(|p| p.len() - 64).collect();
        let _g = names.gate.read().unwrap();
        let t = names.table();
        let mut got = Vec::new();
        for (p, &n) in pool.iter().zip(&lens) {
            let k = unsafe { key_of(p.as_ptr(), n) };
            got.push(unsafe { t.number(&names, &mut supply, &k, spread(&k), p.as_ptr(), n) });
        }
        let mut uniq = got.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), pool.len());
        for (i, (p, &n)) in pool.iter().zip(&lens).enumerate() {
            let k = unsafe { key_of(p.as_ptr(), n) };
            assert_eq!(unsafe { t.number(&names, &mut supply, &k, spread(&k), p.as_ptr(), n) }, got[i]);
            assert_eq!(unsafe { (*names.text(got[i])).bytes() }, &p[..n]);
        }
    }

    #[test]
    fn growing_keeps_every_number() {
        let names = Names::new(4, 1);
        let mut supply = Supply::new();
        let pool: Vec<Vec<u8>> = (0..400_000).map(|i| padded(format!("g{i}").as_bytes())).collect();
        let mut got = Vec::new();
        for chunk in pool.chunks(1000) {
            {
                let _g = names.gate.read().unwrap();
                let t = names.table();
                for p in chunk {
                    let n = p.len() - 64;
                    let k = unsafe { key_of(p.as_ptr(), n) };
                    got.push(unsafe { t.number(&names, &mut supply, &k, spread(&k), p.as_ptr(), n) });
                }
                if !names.report(&mut supply, &t) {
                    continue;
                }
            }
            names.grow();
        }
        let _g = names.gate.read().unwrap();
        let t = names.table();
        assert!(t.mask + 1 > 1 << 18);
        for (i, p) in pool.iter().enumerate() {
            let n = p.len() - 64;
            let k = unsafe { key_of(p.as_ptr(), n) };
            assert_eq!(unsafe { t.number(&names, &mut supply, &k, spread(&k), p.as_ptr(), n) }, got[i]);
        }
    }

    #[test]
    fn head_orders_like_bytes() {
        let items: Vec<&[u8]> = vec![b"U9", b"u1", b"u10", b"u2", b"u", b"u\0", b"abcdefgh", b"abcdefgha", b"abcdefgh\0"];
        for a in &items {
            for b in &items {
                let (pa, pb) = (padded(a), padded(b));
                let ta = Text { head: unsafe { head_of(pa.as_ptr(), a.len()) }, at: pa.as_ptr(), len: a.len() as u32 };
                let tb = Text { head: unsafe { head_of(pb.as_ptr(), b.len()) }, at: pb.as_ptr(), len: b.len() as u32 };
                let lt = if ta.head != tb.head { ta.head < tb.head } else { &pa[..a.len()] < &pb[..b.len()] };
                assert_eq!(lt, a < b, "{a:?} {b:?}");
            }
        }
    }
}
