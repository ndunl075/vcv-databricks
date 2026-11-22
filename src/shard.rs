//! Moves the surviving events and the clicks next to the other rows of the same person.
//!
//! Every person gets a seat: the lead user number, scrambled by a fixed odd multiplier so that
//! busy people spread evenly. The high bits of the seat pick a shard, the low bits number the
//! person inside it. Two sweeps over the records: `tally` turns user numbers into seats, counts
//! the rows per shard and lists the dedup suspects; `deal` copies the rows into one array per
//! kind, shard after shard, through small per-shard buffers so each write is a full cache line.

use crate::dedup::{group_of, Suspect, GROUPS};
use crate::os;
use crate::parse::{Cells, Ck, Ev, Run, NOBODY};

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

/// What one thread found in its share of the records.
pub struct Tally {
    pub events: Vec<u32>,
    pub clicks: Vec<u32>,
    pub suspects: Vec<Vec<Suspect>>,
    /// Indices of the runs this thread tallied; it deals the same ones.
    pub event_runs: Vec<usize>,
    pub click_runs: Vec<usize>,
}

impl Tally {
    pub fn new(shards: usize) -> Tally {
        Tally { events: vec![0; shards], clicks: vec![0; shards], suspects: (0..GROUPS).map(|_| Vec::new()).collect(), event_runs: Vec::new(), click_runs: Vec::new() }
    }
}

const AHEAD: usize = 16;

/// # Safety
/// The run must be live and untouched by other threads; `lead` must cover every user number.
pub unsafe fn tally_events(run: &Run<Ev>, cells: &Cells, lead: &[u32], seats: &Seating, t: &mut Tally) {
    let shift = seats.local_bits;
    for i in 0..run.n {
        let r = run.first.add(i);
        if i + AHEAD < run.n {
            let next = &*run.first.add(i + AHEAD);
            os::soon(cells.word_of(next.tag));
            if next.who != NOBODY {
                os::soon(lead.as_ptr().add(next.who as usize));
            }
        }
        let tag = (*r).tag;
        if cells.crowded(tag) {
            t.suspects.get_unchecked_mut(group_of(tag)).push(Suspect { rec: r, row: run.base.add((*r).at as usize) });
        }
        let who = (*r).who;
        if who != NOBODY {
            let seat = seats.seat(*lead.get_unchecked(who as usize));
            (*r).who = seat;
            *t.events.get_unchecked_mut((seat >> shift) as usize) += 1;
        }
    }
}

/// # Safety
/// As `tally_events`.
pub unsafe fn tally_clicks(run: &Run<Ck>, lead: &[u32], seats: &Seating, t: &mut Tally) {
    let shift = seats.local_bits;
    for i in 0..run.n {
        let r = run.first.add(i);
        if i + AHEAD < run.n {
            os::soon(lead.as_ptr().add((*run.first.add(i + AHEAD)).who as usize));
        }
        let seat = seats.seat(*lead.get_unchecked((*r).who as usize));
        (*r).who = seat;
        *t.clicks.get_unchecked_mut((seat >> shift) as usize) += 1;
    }
}

/// Where each thread's rows of each shard go: `at[thread][shard]` is an index into the dealt
/// array, always a multiple of four rows (one cache line).
pub struct Layout {
    pub at: Vec<Vec<usize>>,
    pub total: usize,
}

impl Layout {
    pub fn new(counts: &[&[u32]], shards: usize) -> Layout {
        let mut at: Vec<Vec<usize>> = counts.iter().map(|_| Vec::with_capacity(shards)).collect();
        let mut next = 0usize;
        for s in 0..shards {
            for (t, c) in counts.iter().enumerate() {
                at[t].push(next);
                next += (c[s] as usize).next_multiple_of(4);
            }
        }
        Layout { at, total: next }
    }
}

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Line([u64; 8]);

/// One thread's buffers for dealing: a cache line per shard.
pub struct Dealer {
    lines: Vec<Line>,
    cursor: Vec<usize>,
}

impl Dealer {
    pub fn new(shards: usize) -> Dealer {
        Dealer { lines: vec![Line([0; 8]); shards], cursor: vec![0; shards] }
    }

    fn start(&mut self, at: &[usize]) {
        self.cursor.copy_from_slice(at);
    }

    #[inline(always)]
    unsafe fn put(&mut self, out: *mut Row, shard: usize, row: Row) {
        let c = *self.cursor.get_unchecked(shard);
        let line = self.lines.get_unchecked_mut(shard);
        let k = c & 3;
        *line.0.get_unchecked_mut(k * 2) = row[0];
        *line.0.get_unchecked_mut(k * 2 + 1) = row[1];
        *self.cursor.get_unchecked_mut(shard) = c + 1;
        if k == 3 {
            send_line(out.add(c - 3) as *mut u8, line);
        }
    }

    /// Writes out the partly filled lines; returns how many rows each shard received.
    unsafe fn finish(&mut self, out: *mut Row, at: &[usize]) -> Vec<u32> {
        let mut got = Vec::with_capacity(at.len());
        for (s, &start) in at.iter().enumerate() {
            let c = self.cursor[s];
            let k = c & 3;
            if k > 0 {
                std::ptr::copy_nonoverlapping(self.lines[s].0.as_ptr() as *const Row, out.add(c - k), k);
            }
            got.push((c - start) as u32);
        }
        #[cfg(target_arch = "x86_64")]
        std::arch::x86_64::_mm_sfence();
        got
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

/// Bits needed to hold any value below `span` (at least one).
pub fn bits_for(span: i64) -> u32 {
    (64 - (span.max(2) as u64 - 1).leading_zeros()).max(1)
}

/// Deals the valid, surviving events of `runs`. A dealt event is
/// `[person-in-shard << time_bits | milliseconds after the window start, price | category << 32]`,
/// so ordering a shard's rows by the first word groups them by person, in time order.
/// Returns the rows per shard.
///
/// # Safety
/// `out` must have room at every `at[shard]` for the rows this thread tallied there.
pub unsafe fn deal_events(runs: &[&Run<Ev>], seats: &Seating, time_bits: u32, d: &mut Dealer, out: *mut Row, at: &[usize]) -> Vec<u32> {
    d.start(at);
    let shift = seats.local_bits;
    let low = (1u32 << shift) - 1;
    for run in runs {
        for i in 0..run.n {
            let r = &*run.first.add(i);
            if r.who != NOBODY {
                let person = (r.who & low) as u64;
                d.put(out, (r.who >> shift) as usize, [person << time_bits | r.key >> 24, (r.key & 0xff_ffff) | (r.cat as u64) << 32]);
            }
        }
    }
    d.finish(out, at)
}

/// Deals clicks. A dealt click is
/// `[person-in-shard << time_bits | milliseconds after the click window start, campaign | number << 32]`,
/// where `number` is the click's place in the list of all click records.
///
/// # Safety
/// As `deal_events`.
pub unsafe fn deal_clicks(runs: &[(&Run<Ck>, u32)], seats: &Seating, time_bits: u32, d: &mut Dealer, out: *mut Row, at: &[usize]) -> Vec<u32> {
    d.start(at);
    let shift = seats.local_bits;
    let low = (1u32 << shift) - 1;
    for (run, first) in runs {
        for i in 0..run.n {
            let r = &*run.first.add(i);
            let person = (r.who & low) as u64;
            d.put(out, (r.who >> shift) as usize, [person << time_bits | r.when, r.camp as u64 | ((*first as u64 + i as u64) << 32)]);
        }
    }
    d.finish(out, at)
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
