//! Dedup on event_id (smallest ingest_ts wins, before validation) for one shard.

use crate::buckets::Buckets;
use crate::rec::*;
use crate::util::field_at;
use std::cmp::Ordering;

#[inline]
fn same_id(a: u64, b: u64) -> bool {
    let (pa, pb) = (a & PTR_MASK, b & PTR_MASK);
    pa == pb || unsafe { field_at(pa) == field_at(pb) }
}

/// Total order on copies of one event: smallest ingest_ts first; the rest only makes the
/// choice deterministic if two copies with equal ingest_ts ever differ.
fn better(a: &Ev, b: &Ev) -> Ordering {
    (a.ing, a.ep >> 63, a.ts, a.ulo, a.uhi, a.price, a.cat).cmp(&(b.ing, b.ep >> 63, b.ts, b.ulo, b.uhi, b.price, b.cat))
}

/// Keeps one copy per event_id. `bufs` are all records of one shard.
pub fn dedup(bufs: &[Buckets<Ev>]) -> Vec<Ev> {
    let total: usize = bufs.iter().map(|b| b.len()).sum();
    let cap = (total * 2).next_power_of_two().max(16);
    let mask = cap - 1;
    let mut table = vec![0u32; cap];
    let mut kept: Vec<Ev> = Vec::with_capacity(total);
    for b in bufs.iter().flat_map(|b| b.blocks()) {
        for r in b {
            let mut i = (r.eh as usize) & mask;
            loop {
                let t = table[i];
                if t == 0 {
                    kept.push(*r);
                    table[i] = kept.len() as u32;
                    break;
                }
                let k = &mut kept[(t - 1) as usize];
                if k.eh == r.eh && same_id(k.ep, r.ep) {
                    if better(r, k) == Ordering::Less {
                        *k = *r;
                    }
                    break;
                }
                i = (i + 1) & mask;
            }
        }
    }
    kept
}
