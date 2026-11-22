//! One range of hash cells at a time: mark the cells, dedup exactly, deal the survivors.
//!
//! A suspect is an event whose hash cell was marked by more than one row. Copies of one event
//! share their hash, and all of them are suspects. Suspects are matched by the full 64-bit hash,
//! and every match is confirmed on the event_id bytes (never a hash alone). In each set of copies
//! the one with the smallest ingest_ts stays (the first one seen on a tie); the others lose
//! their user number. An invalid copy takes part like any other, so it can be the one that
//! stays, which drops the event.

use crate::os;
use crate::parse::{Cells, Ev, NOBODY};
use crate::shard::{Deal, Dealer};

/// Scratch space for `range`.
#[derive(Default)]
pub struct Desk {
    found: Vec<*mut Ev>,
    /// Open addressing over `found`: index + 1, 0 = free.
    slots: Vec<u32>,
    /// (first copy, later copy) for every hash seen again, in the order seen.
    pairs: Vec<(u32, u32)>,
}

// SAFETY: the pointers are only scratch between calls.
unsafe impl Send for Desk {}

/// Value of the field starting at `p` in a well-formed row.
#[inline(always)]
unsafe fn field(p: *const u8) -> (*const u8, usize) {
    let (start, close) = if *p == b'"' { (p.add(1), b'"') } else { (p, b',') };
    let mut n = 0usize;
    while *start.add(n) != close {
        n += 1;
    }
    (start, n)
}

#[inline(always)]
unsafe fn same_id(a: *const u8, b: *const u8) -> bool {
    let (x, xn) = field(a);
    let (y, yn) = field(b);
    std::slice::from_raw_parts(x, xn) == std::slice::from_raw_parts(y, yn)
}

const AHEAD: usize = 16;

/// Handles one range of cells, whose records are the stretches in `parts`: marks the cells,
/// settles the copies, deals the valid survivors into `events`. Returns how many suspects it saw.
///
/// # Safety
/// The stretches must hold live records of this range only, used by no other thread meanwhile;
/// `deal.lead` must cover every user number.
pub unsafe fn range(parts: &[(*mut Ev, usize)], cells: &Cells, desk: &mut Desk, deal: &Deal, events: &mut Dealer) -> usize {
    for &(first, n) in parts {
        for i in 0..n {
            if i + AHEAD < n {
                os::soon(cells.word_of((*first.add(i + AHEAD)).tag()));
            }
            cells.mark((*first.add(i)).tag());
        }
    }
    let found = &mut desk.found;
    found.clear();
    for &(first, n) in parts {
        for i in 0..n {
            let r = first.add(i);
            if cells.crowded((*r).tag()) {
                found.push(r);
            }
        }
    }
    let suspects = found.len();
    if suspects >= 2 {
        settle(desk);
    }
    for &(first, n) in parts {
        for i in 0..n {
            if i + AHEAD < n {
                let w = (*first.add(i + AHEAD)).who;
                if w != NOBODY {
                    os::soon(deal.lead.as_ptr().add(w as usize));
                }
            }
            let r = &*first.add(i);
            if r.who != NOBODY {
                deal.event(r, events);
            }
        }
    }
    suspects
}

unsafe fn settle(desk: &mut Desk) {
    let found = &mut desk.found;
    let n = found.len();
    let cap = (n * 2).next_power_of_two();
    let mask = cap - 1;
    desk.slots.clear();
    desk.slots.resize(cap, 0);
    desk.pairs.clear();
    for i in 0..n {
        let h = (**found.get_unchecked(i)).hash;
        let mut j = (h as usize) & mask;
        loop {
            let t = *desk.slots.get_unchecked(j);
            if t == 0 {
                *desk.slots.get_unchecked_mut(j) = i as u32 + 1;
                break;
            }
            if (**found.get_unchecked(t as usize - 1)).hash == h {
                desk.pairs.push((t - 1, i as u32));
                break;
            }
            j = (j + 1) & mask;
        }
    }
    if desk.pairs.is_empty() {
        return;
    }
    // Confirm every match on the bytes; the rows are far apart, so ask for them ahead.
    let pairs = &desk.pairs;
    let mut all_same = true;
    for (k, &(a, b)) in pairs.iter().enumerate() {
        if let Some(&(x, y)) = pairs.get(k + 8) {
            os::soon((**found.get_unchecked(x as usize)).row);
            os::soon((**found.get_unchecked(y as usize)).row);
        }
        all_same &= same_id((**found.get_unchecked(a as usize)).row, (**found.get_unchecked(b as usize)).row);
    }
    if !all_same {
        exact(found);
        return;
    }
    for &(a, b) in pairs {
        let c = *found.get_unchecked(b as usize);
        let w = found.get_unchecked_mut(a as usize);
        if (*c).ingest < (**w).ingest {
            (**w).who = NOBODY;
            *w = c;
        } else {
            (*c).who = NOBODY;
        }
    }
}

/// The same, for the rare range where two different event_ids share a 64-bit hash.
#[cold]
#[inline(never)]
unsafe fn exact(found: &[*mut Ev]) {
    let mut order: Vec<u32> = (0..found.len() as u32).collect();
    order.sort_by_key(|&i| ((*found[i as usize]).hash, i));
    let mut i = 0;
    while i < order.len() {
        let mut j = i + 1;
        while j < order.len() && (*found[order[j] as usize]).hash == (*found[order[i] as usize]).hash {
            j += 1;
        }
        // One hash, in the order seen: split by event_id, keep the first smallest ingest of each.
        let mut winners: Vec<*mut Ev> = Vec::new();
        for &k in &order[i..j] {
            let c = found[k as usize];
            match winners.iter_mut().find(|w| same_id((***w).row, (*c).row)) {
                Some(w) => {
                    if (*c).ingest < (**w).ingest {
                        (**w).who = NOBODY;
                        *w = c;
                    } else {
                        (*c).who = NOBODY;
                    }
                }
                None => winners.push(c),
            }
        }
        i = j;
    }
}
