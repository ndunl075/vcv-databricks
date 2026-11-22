//! Moves the events and the clicks next to the other rows of the same person.
//!
//! Every person gets a seat: the lead user number, scrambled by a fixed odd multiplier so that
//! busy people spread evenly. The high bits of the seat pick a shard, the low bits number the
//! person inside it. Rows go into their shard's blocks through a small per-shard buffer, so each
//! write is a full cache line. Valid events are dealt as they are read (links are read first, so
//! every user's lead is known by then); a copy that loses dedup is flagged in its dealt row
//! (`DROPPED`) and skipped later. Clicks are dealt by a sweep after reading.

use crate::os;
use crate::parse::{Ck, Run};

/// Set in the second word of a dealt event that lost dedup.
pub const DROPPED: u64 = 1 << 63;

/// A dealt row: two words (see `deal_events` and `deal_clicks`).
pub type Row = [u64; 2];

pub struct Seating {
    mul: u32,
    back: u32,
    mask: u32,
    /// Low bits of a seat that number the person inside the shard.
    pub local_bits: u32,
    pub shards: usize,
}

impl Seating {
    /// `ids`: one more than the largest user number; `rows`: how many rows will be dealt.
    pub fn new(ids: usize, rows: usize) -> Seating {
        let bits = (ids.max(2) as u64).next_power_of_two().trailing_zeros();
        let want = ((rows / 6000).max(1) as u64).next_power_of_two().trailing_zeros().clamp(4, 12);
        let shard_bits = want.max(bits.saturating_sub(24)).min(bits);
        let mul: u32 = 0x9e37_79b1;
        // Inverse of `mul` modulo 2^32 by Newton's iteration.
        let mut back: u32 = mul;
        for _ in 0..5 {
            back = back.wrapping_mul(2u32.wrapping_sub(mul.wrapping_mul(back)));
        }
        Seating { mul, back, mask: if bits == 32 { u32::MAX } else { (1u32 << bits) - 1 }, local_bits: bits - shard_bits, shards: 1usize << shard_bits }
    }
    #[inline(always)]
    pub fn seat(&self, lead: u32) -> u32 {
        lead.wrapping_mul(self.mul) & self.mask
    }
    /// The lead number sitting at `seat`.
    #[inline(always)]
    pub fn lead(&self, seat: u32) -> u32 {
        seat.wrapping_mul(self.back) & self.mask
    }
}

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Line([u64; 8]);

/// Rows per block of dealt rows (16 cache lines).
const BLOCK_ROWS: usize = 64;
/// Blocks taken from the system at a time.
const BLOCKS_PER_GRAB: usize = 8192;

/// A filled (or partly filled) block of one shard's rows.
#[derive(Clone, Copy)]
pub struct Block {
    pub shard: u32,
    pub n: u32,
    pub at: *const Row,
}

/// One thread's dealing of one kind of row: per shard a cache line being filled and a block
/// being written, so each write to the far-away blocks is a whole line.
pub struct Dealer {
    lines: Vec<Line>,
    /// Next free row of each shard's current block (null before the first row).
    next: Vec<*mut Row>,
    end: Vec<*mut Row>,
    pub blocks: Vec<Block>,
    spare: *mut Row,
    spare_blocks: usize,
}

impl Dealer {
    pub fn new(shards: usize) -> Dealer {
        Dealer { lines: vec![Line([0; 8]); shards], next: vec![std::ptr::null_mut(); shards], end: vec![std::ptr::null_mut(); shards], blocks: Vec::new(), spare: std::ptr::null_mut(), spare_blocks: 0 }
    }

    #[cold]
    #[inline(never)]
    fn new_block(&mut self, shard: usize) -> *mut Row {
        let old_end = self.end[shard];
        if !old_end.is_null() {
            // SAFETY: the block starts BLOCK_ROWS before its end.
            self.blocks.push(Block { shard: shard as u32, n: BLOCK_ROWS as u32, at: unsafe { old_end.sub(BLOCK_ROWS) } });
        }
        if self.spare_blocks == 0 {
            self.spare = os::zeroed_array::<Row>(BLOCK_ROWS * BLOCKS_PER_GRAB);
            self.spare_blocks = BLOCKS_PER_GRAB;
        }
        let at = self.spare;
        // SAFETY: the grab holds `spare_blocks` more blocks.
        unsafe {
            self.spare = at.add(BLOCK_ROWS);
            self.end[shard] = at.add(BLOCK_ROWS);
        }
        self.spare_blocks -= 1;
        at
    }

    /// Adds a row to a shard; returns where the row will be once the dealer is finished.
    #[inline(always)]
    pub fn put(&mut self, shard: usize, row: Row) -> *mut Row {
        // SAFETY: `shard` is below the shard count; blocks are 64-byte aligned, so the low bits
        // of a row address say which quarter of its cache line it is.
        unsafe {
            let mut at = *self.next.get_unchecked(shard);
            if at == *self.end.get_unchecked(shard) {
                at = self.new_block(shard);
            }
            let k = (at as usize >> 4) & 3;
            let line = self.lines.get_unchecked_mut(shard);
            *line.0.get_unchecked_mut(k * 2) = row[0];
            *line.0.get_unchecked_mut(k * 2 + 1) = row[1];
            *self.next.get_unchecked_mut(shard) = at.add(1);
            if k == 3 {
                send_line(at.sub(3) as *mut u8, line);
            }
            at
        }
    }

    /// Writes out the partly filled lines and closes every block.
    pub fn finish(&mut self) {
        for s in 0..self.lines.len() {
            let at = self.next[s];
            if at.is_null() {
                continue;
            }
            // SAFETY: the pointers lie in this shard's current block.
            unsafe {
                let k = (at as usize >> 4) & 3;
                if k > 0 {
                    std::ptr::copy_nonoverlapping(self.lines[s].0.as_ptr() as *const Row, at.sub(k), k);
                }
                let start = self.end[s].sub(BLOCK_ROWS);
                let n = at.offset_from(start) as usize;
                if n > 0 {
                    self.blocks.push(Block { shard: s as u32, n: n as u32, at: start });
                }
            }
            self.next[s] = std::ptr::null_mut();
            self.end[s] = std::ptr::null_mut();
        }
        #[cfg(target_arch = "x86_64")]
        // SAFETY: plain store fence after the streaming stores.
        unsafe {
            std::arch::x86_64::_mm_sfence()
        };
    }
}

/// Copies a full line to its 64-byte aligned place without first reading that place.
#[inline(always)]
unsafe fn send_line(to: *mut u8, line: &Line) {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::*;
        let src = line.0.as_ptr() as *const __m128i;
        let dst = to as *mut __m128i;
        _mm_stream_si128(dst, _mm_load_si128(src));
        _mm_stream_si128(dst.add(1), _mm_load_si128(src.add(1)));
        _mm_stream_si128(dst.add(2), _mm_load_si128(src.add(2)));
        _mm_stream_si128(dst.add(3), _mm_load_si128(src.add(3)));
    }
    #[cfg(not(target_arch = "x86_64"))]
    std::ptr::copy_nonoverlapping(line.0.as_ptr() as *const u8, to, 64);
}

/// All dealt rows of one kind, shard by shard.
pub struct Dealt {
    blocks: Vec<(*const Row, u32)>,
    /// Blocks of shard `s` are `blocks[first[s]..first[s + 1]]`.
    first: Vec<u32>,
    rows: Vec<u32>,
}

// SAFETY: the blocks are only read once dealing is over.
unsafe impl Sync for Dealt {}

impl Dealt {
    pub fn new(lists: &[&[Block]], shards: usize) -> Dealt {
        let mut first = vec![0u32; shards + 1];
        let mut rows = vec![0u32; shards];
        for b in lists.iter().flat_map(|l| l.iter()) {
            first[b.shard as usize + 1] += 1;
            rows[b.shard as usize] += b.n;
        }
        for s in 0..shards {
            first[s + 1] += first[s];
        }
        let mut blocks = vec![(std::ptr::null::<Row>(), 0u32); first[shards] as usize];
        let mut at = first.clone();
        for b in lists.iter().flat_map(|l| l.iter()) {
            blocks[at[b.shard as usize] as usize] = (b.at, b.n);
            at[b.shard as usize] += 1;
        }
        Dealt { blocks, first, rows }
    }
    pub fn count(&self, shard: usize) -> usize {
        self.rows[shard] as usize
    }
    pub fn parts(&self, shard: usize) -> impl Iterator<Item = &[Row]> + Clone + '_ {
        // SAFETY: every block holds `n` rows that were written before dealing finished.
        self.blocks[self.first[shard] as usize..self.first[shard + 1] as usize].iter().map(|&(at, n)| unsafe { std::slice::from_raw_parts(at, n as usize) })
    }
}

/// One thread's results of the click sweep.
pub struct Hand {
    pub clicks: Dealer,
}

// SAFETY: a hand is used by one thread at a time.
unsafe impl Send for Hand {}

impl Hand {
    pub fn new(shards: usize) -> Hand {
        Hand { clicks: Dealer::new(shards) }
    }
}

/// What every sweep needs to turn a record into a dealt row.
pub struct Deal<'a> {
    pub seats: &'a Seating,
    /// The person's lead number, for every user number below its length; a larger number is
    /// its own lead (it was handed out after the links were joined, so it is in no link).
    pub lead: &'a [u32],
    pub event_time_bits: u32,
    pub click_time_bits: u32,
}

impl Deal<'_> {
    #[inline(always)]
    fn lead_of(&self, who: u32) -> u32 {
        match self.lead.get(who as usize) {
            Some(&l) => l,
            None => who,
        }
    }

    /// Deals one valid event of user number `who`: `key` is milliseconds after the window
    /// start shifted left 24 plus the price, `cat` the category rank. A dealt event is
    /// `[person-in-shard << time_bits | milliseconds after the window start, price | category << 32]`,
    /// so ordering a shard's rows by the first word groups them by person, in time order.
    /// Returns where the row will be.
    #[inline(always)]
    pub fn event(&self, who: u32, key: u64, cat: u32, d: &mut Dealer) -> *mut Row {
        let bits = self.seats.local_bits;
        let seat = self.seats.seat(self.lead_of(who));
        let person = (seat & ((1u32 << bits) - 1)) as u64;
        d.put((seat >> bits) as usize, [person << self.event_time_bits | key >> 24, (key & 0xff_ffff) | (cat as u64) << 32])
    }
}

const AHEAD: usize = 16;

/// Deals the clicks of a run. A dealt click is
/// `[person-in-shard << time_bits | milliseconds after the click window start, campaign | number << 32]`,
/// where `number` is the click's place in the list of all click records (`first` for the run's
/// first record).
///
/// # Safety
/// As `sweep_events`.
pub unsafe fn sweep_clicks(run: &Run<Ck>, first: u32, deal: &Deal, h: &mut Hand) {
    let bits = deal.seats.local_bits;
    let low = (1u32 << bits) - 1;
    for i in 0..run.n {
        let r = &*run.first.add(i);
        if i + AHEAD < run.n {
            os::soon(deal.lead.as_ptr().add((*run.first.add(i + AHEAD)).who as usize));
        }
        let seat = deal.seats.seat(deal.lead_of(r.who));
        h.clicks.put((seat >> bits) as usize, [((seat & low) as u64) << deal.click_time_bits | r.when, r.camp as u64 | ((first as u64 + i as u64) << 32)]);
    }
}

/// Bits needed to hold any value below `span` (at least one).
pub fn bits_for(span: i64) -> u32 {
    (64 - (span.max(2) as u64 - 1).leading_zeros()).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seats_are_a_reversible_shuffle() {
        for ids in [1usize, 2, 3, 1000, 4097, 1 << 20, (1 << 20) + 1] {
            let s = Seating::new(ids, 50_000_000);
            let mut seen = std::collections::HashSet::new();
            for lead in (0..ids as u32).step_by((ids / 5000).max(1)) {
                let seat = s.seat(lead);
                assert!((seat >> s.local_bits) < s.shards as u32);
                assert_eq!(s.lead(seat), lead);
                assert!(seen.insert(seat));
            }
            assert!(s.local_bits <= 24);
        }
        let s = Seating::new(u32::MAX as usize, 10);
        assert!(s.local_bits <= 24);
        assert_eq!(s.lead(s.seat(4_000_000_000)), 4_000_000_000);
    }
}
