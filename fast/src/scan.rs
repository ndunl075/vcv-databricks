//! Runs: maximal stretches of consecutive valid slots of one user number.
//!
//! The generator hands out event and click numbers per user in order, so a user normally
//! owns exactly one run. Nothing relies on that: a user with two or more runs (or any other
//! irregularity) is counted here and sent to the slow path.

use crate::fx::*;
use std::sync::atomic::Ordering::*;

#[derive(Clone, Copy)]
pub struct Run {
    pub u: u32,
    pub s: u32,
    pub e: u32,
}

/// Runs that start in `[lo, hi)`; a run that reaches `hi` is extended past it, and a run that
/// began before `lo` is left to its owner. `valid(i)` is the user number of slot `i`, if valid.
fn runs_in(lo: usize, hi: usize, nmax: usize, valid: impl Fn(usize) -> Option<u32>) -> Vec<Run> {
    let mut prev = None;
    let mut j = lo;
    while j > 0 {
        j -= 1;
        if let Some(u) = valid(j) {
            prev = Some(u);
            break;
        }
    }
    let mut skipping = prev;
    let mut out = Vec::new();
    let mut cur: Option<(u32, u32, u32)> = None;
    for i in lo..hi {
        let Some(u) = valid(i) else { continue };
        if let Some(pu) = skipping {
            if u == pu {
                continue;
            }
            skipping = None;
        }
        match &mut cur {
            Some((cu, _, last)) if *cu == u => *last = i as u32,
            _ => {
                if let Some((cu, s, l)) = cur {
                    out.push(Run { u: cu, s, e: l + 1 });
                }
                cur = Some((u, i as u32, i as u32));
            }
        }
    }
    if let Some((cu, s, mut last)) = cur {
        let mut j = hi;
        while j < nmax {
            match valid(j) {
                Some(u) if u == cu => last = j as u32,
                Some(_) => break,
                None => {}
            }
            j += 1;
        }
        out.push(Run { u: cu, s, e: last + 1 });
    }
    out
}

fn count(cnts: &[std::sync::atomic::AtomicU8], fx: &Fx, runs: &[Run], slow: &mut Vec<u32>) {
    for r in runs {
        let c = &cnts[r.u as usize];
        let p = c.load(Relaxed);
        if p < 2 {
            let was = c.fetch_add(1, Relaxed);
            if was == 1 && fx.slow[r.u as usize].swap(1, Relaxed) == 0 {
                slow.push(r.u);
            }
        }
    }
}

/// Event runs of block `b` of `BLOCK` slots; `nmax` is one past the highest used slot.
pub fn event_runs(fx: &Fx, b: usize, nmax: usize, slow: &mut Vec<u32>) -> Vec<Run> {
    let lo = b * BLOCK;
    let hi = (lo + BLOCK).min(nmax);
    let valid = |i: usize| {
        let r = &fx.ev[i];
        if r.ing.load(Relaxed) == 0 || r.ts.load(Relaxed) == INVALID {
            None
        } else {
            Some(r.v.load(Relaxed) >> 9)
        }
    };
    let runs = runs_in(lo, hi, nmax, valid);
    count(fx.evcnt, fx, &runs, slow);
    runs
}

/// Click runs of block `b`; also records each user's run for the lookup by user number.
pub fn click_runs(fx: &Fx, b: usize, nmax: usize, slow: &mut Vec<u32>) -> Vec<Run> {
    let lo = b * BLOCK;
    let hi = (lo + BLOCK).min(nmax);
    let valid = |i: usize| {
        let r = &fx.ck[i];
        if r.camp.load(Relaxed) == 0 {
            None
        } else {
            Some(r.v.load(Relaxed) >> 9)
        }
    };
    let runs = runs_in(lo, hi, nmax, valid);
    count(fx.ckcnt, fx, &runs, slow);
    for r in &runs {
        fx.crun[r.u as usize].store((r.s as u64) << 32 | r.e as u64, Relaxed);
    }
    runs
}
