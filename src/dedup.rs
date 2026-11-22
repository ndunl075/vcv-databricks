//! Exact dedup on event_id for the events that may have copies.
//!
//! A suspect is an event whose hash cell was marked by more than one row. Suspects are grouped
//! by the event_id itself (bytes, never a hash alone); in each group the copy with the smallest
//! ingest_ts stays and every other copy is switched off (`who = NOBODY`). An invalid copy takes
//! part like any other, so it can be the one that stays, which drops the event.

use crate::ids::{is_long, key_of, spread, Key};
use crate::os;
use crate::parse::{Ev, NOBODY};
use crate::stamp::{Dates, NEVER};

#[derive(Clone, Copy)]
pub struct Suspect {
    pub rec: *mut Ev,
    /// First byte of the event's row.
    pub row: *const u8,
}

/// Suspects are dealt into this many groups by hash, so groups can be settled side by side.
pub const GROUPS: usize = 256;

#[inline(always)]
pub fn group_of(tag: u32) -> usize {
    (tag >> 24) as usize
}

/// Value of the field starting at `p` in a well-formed row, and where the next field starts.
#[inline(always)]
unsafe fn field(p: *const u8) -> (*const u8, usize, *const u8) {
    let (start, close) = if *p == b'"' { (p.add(1), b'"') } else { (p, b',') };
    let mut n = 0usize;
    while *start.add(n) != close {
        n += 1;
    }
    (start, n, start.add(n + 1 + (close == b'"') as usize))
}

/// The event_id and ingest_ts of the event row at `row`.
#[inline(always)]
unsafe fn id_and_ingest(row: *const u8, dates: &mut Dates) -> (*const u8, usize, i64) {
    let (id, id_len, user) = field(row);
    let (_, _, ts) = field(user);
    let (_, _, ingest) = field(ts);
    let (at, n, _) = field(ingest);
    let t = dates.instant(at, n);
    // An unreadable ingest_ts sorts last (the event is invalid either way).
    (id, id_len, if t == NEVER { i64::MAX } else { t })
}

#[derive(Clone, Copy)]
pub struct Kept {
    key: Key,
    ingest: i64,
    rec: *mut Ev,
    row: *const u8,
}

const FREE: Kept = Kept { key: [0; 3], ingest: 0, rec: std::ptr::null_mut(), row: std::ptr::null() };

/// Settles one group. `table` is scratch space.
///
/// # Safety
/// Every suspect must point at a live record and at the start of the row it was read from, and
/// no other thread may touch this group's records.
pub unsafe fn settle(lists: &[&[Suspect]], table: &mut Vec<Kept>, dates: &mut Dates) {
    let total: usize = lists.iter().map(|l| l.len()).sum();
    if total < 2 {
        return;
    }
    let cap = (total * 2).next_power_of_two();
    let mask = cap - 1;
    table.clear();
    table.resize(cap, FREE);
    for list in lists {
        for (i, s) in list.iter().enumerate() {
            if let Some(ahead) = list.get(i + 8) {
                os::soon(ahead.row);
                os::soon(ahead.rec);
            }
            let (id, id_len, ingest) = id_and_ingest(s.row, dates);
            let key = key_of(id, id_len);
            let mut j = (spread(&key) as usize) & mask;
            loop {
                let k = table.get_unchecked_mut(j);
                if k.rec.is_null() {
                    *k = Kept { key, ingest, rec: s.rec, row: s.row };
                    break;
                }
                if k.key == key && (!is_long(&key) || same_id(k.row, id, id_len)) {
                    if ingest < k.ingest {
                        (*k.rec).who = NOBODY;
                        k.ingest = ingest;
                        k.rec = s.rec;
                        k.row = s.row;
                    } else {
                        (*s.rec).who = NOBODY;
                    }
                    break;
                }
                j = (j + 1) & mask;
            }
        }
    }
}

#[cold]
#[inline(never)]
unsafe fn same_id(row: *const u8, id: *const u8, id_len: usize) -> bool {
    let (other, other_len, _) = field(row);
    std::slice::from_raw_parts(other, other_len) == std::slice::from_raw_parts(id, id_len)
}
