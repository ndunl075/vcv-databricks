//! Line parsers: events and clicks go straight into their dense slots by number; anything
//! that is not a plain generator row goes to a per-thread "extras" list, handled exactly
//! (and slowly) later. Rules are those of rules.md; see src/parse.rs of the general pipeline
//! for the same checks on hashed keys.

use crate::csv::for_each_row;
use crate::fx::*;
use crate::handoff::give_up;
use crate::idc::{code_of, number};
use crate::ts::{parse_price, parse_ts};
use crate::util::DAY_MS;
use std::sync::atomic::Ordering::*;
use std::sync::atomic::AtomicU64;

/// An event row that is not a generator row (or whose number is out of range).
pub struct ExEv {
    pub eid: &'static [u8],
    pub user: &'static [u8],
    pub ing: i64,
    pub ts: i64,
    pub price: u32,
    pub cat: u32,
    pub ok: bool,
    /// Already deduplicated (an odd-user winner out of the dense array).
    pub pre: bool,
}

pub struct ExCk {
    pub cid: &'static [u8],
    pub user: &'static [u8],
    pub ts: i64,
    pub camp: u32,
}

pub struct ExLink {
    pub a: &'static [u8],
    pub b: &'static [u8],
}

/// A dense event copy whose user is not a plain generator user; the bytes wait here.
pub struct OddUser {
    pub n: u32,
    pub ingw: u32,
    pub user: &'static [u8],
}

#[derive(Default)]
pub struct Tl {
    pub ex_ev: Vec<ExEv>,
    pub ex_ck: Vec<ExCk>,
    pub ex_lk: Vec<ExLink>,
    pub odd: Vec<OddUser>,
    pub slow: Vec<u32>,
    pub nmax_ev: usize,
    pub nmax_ck: usize,
    pub sent: (usize, usize, usize),
}

#[inline]
pub fn word_find(w: u64, mut a: u32) -> u32 {
    loop {
        let p = (((w >> (4 * a)) & 15) as u32) ^ a;
        if p == a {
            return a;
        }
        a = p;
    }
}

fn word_union(cell: &AtomicU64, a: u32, b: u32) {
    let mut old = cell.load(Relaxed);
    loop {
        let (ra, rb) = (word_find(old, a), word_find(old, b));
        if ra == rb {
            return;
        }
        let (hi, lo) = if ra > rb { (ra, rb) } else { (rb, ra) };
        let new = old | (((lo ^ hi) as u64) << (4 * hi));
        match cell.compare_exchange_weak(old, new, Relaxed, Relaxed) {
            Ok(_) => return,
            Err(x) => old = x,
        }
    }
}

#[inline]
pub fn flag_slow(fx: &Fx, tl: &mut Tl, u: usize) {
    let c = &fx.slow[u];
    if c.load(Relaxed) == 0 && c.swap(1, Relaxed) == 0 {
        tl.slow.push(u as u32);
    }
}

/// `v` of a plain user ID: number * 512 + alias with number below `UCAP`.
#[inline]
fn user_v(fx: &Fx, b: &[u8]) -> Option<u64> {
    let c = code_of(b, b'u')?;
    let v = number(c, fx.u_key);
    ((v >> 9) < UCAP).then_some(v)
}

#[inline(always)]
fn write_slot(r: &EvRec, ingw: u32, ts: u32, v: u32, pc: u32) {
    loop {
        let cur = r.ing.load(Acquire);
        if cur == LOCK {
            std::hint::spin_loop();
            continue;
        }
        if cur != 0 && ingw >= cur {
            return; // the kept copy stays (equal ingest: identical by the rules)
        }
        if r.ing.compare_exchange(cur, LOCK, Acquire, Relaxed).is_ok() {
            r.ts.store(ts, Relaxed);
            r.v.store(v, Relaxed);
            r.pc.store(pc, Relaxed);
            r.ing.store(ingw, Release);
            return;
        }
    }
}

pub fn parse_events(fx: &Fx, tl: &mut Tl, data: &'static [u8], s: usize, e: usize) {
    let (ws, we) = (fx.win.ws, fx.win.we);
    let mut batch = [Pending { n: 0, ingw: 0, ts: 0, v: 0, pc: 0 }; BATCH];
    let mut nb = 0usize;
    for_each_row::<7>(data, s, e, |f| {
        let eid = &data[f[0].0..f[0].1];
        if eid.is_empty() {
            return;
        }
        let user = &data[f[1].0..f[1].1];
        let ts = parse_ts(&data[f[2].0..f[2].1]);
        let ing = parse_ts(&data[f[3].0..f[3].1]);
        let ty = &data[f[4].0..f[4].1];
        let prod = &data[f[5].0..f[5].1];

        let mut ok = !user.is_empty() && !prod.is_empty();
        let ts_v = match ts {
            Some(t) if t >= ws && t < we => t,
            _ => {
                ok = false;
                0
            }
        };
        let ing_v = match ing {
            Some(i) => i,
            None => {
                ok = false;
                i64::MAX
            }
        };
        let mut price = 0u32;
        let mut cat = 0u32;
        match ty {
            b"view" | b"cart" => {}
            b"purchase" => match parse_price(&data[f[6].0..f[6].1]) {
                Some(p) => {
                    price = p;
                    cat = match fx.prods.map.get(prod) {
                        Some(pi) => fx.prods.cat_of[pi as usize],
                        None => fx.prods.unknown,
                    };
                }
                None => ok = false,
            },
            _ => ok = false,
        }
        if ok && ing_v.checked_sub(ts_v).map_or(true, |d| d > DAY_MS) {
            ok = false;
        }

        let n = code_of(eid, b'e').map(|c| number(c, fx.ev_key)).filter(|&n| (n as usize) < fx.ev_cap);
        let Some(n) = n else {
            tl.ex_ev.push(ExEv { eid, user, ing: ing_v, ts: ts_v, price, cat, ok, pre: false });
            return;
        };
        let n = n as usize;
        // Ingest rank for dedup: whole seconds in range, or the worst rank for a bad ingest_ts.
        let ingw = if ing_v == i64::MAX {
            BAD_ING
        } else {
            let sec = ing_v.div_euclid(1000) - fx.ing_base;
            if ing_v.rem_euclid(1000) != 0 || sec < 0 || sec >= (1 << 31) {
                give_up("ingest_ts not whole seconds near the window");
            }
            (sec + 1) as u32
        };
        let (tsr, v, pc) = if ok {
            let v = match user_v(fx, user) {
                Some(v) => {
                    if (v & 511) as u32 >= NALIAS {
                        flag_slow(fx, tl, (v >> 9) as usize);
                    }
                    v as u32
                }
                None => {
                    tl.odd.push(OddUser { n: n as u32, ingw, user });
                    ODD
                }
            };
            ((ts_v - ws) as u32, v, price << 8 | cat)
        } else {
            (INVALID, 0, 0)
        };
        if n >= tl.nmax_ev {
            tl.nmax_ev = n + 1;
        }
        // Slot writes use a locked instruction, which waits for the cache line: prefetch now,
        // write a batch later.
        prefetch(&fx.ev[n]);
        batch[nb] = Pending { n: n as u32, ingw, ts: tsr, v, pc };
        nb += 1;
        if nb == BATCH {
            for p in &batch {
                write_slot(&fx.ev[p.n as usize], p.ingw, p.ts, p.v, p.pc);
            }
            nb = 0;
        }
    });
    for p in &batch[..nb] {
        write_slot(&fx.ev[p.n as usize], p.ingw, p.ts, p.v, p.pc);
    }
    flush_counts(fx, tl);
}

const BATCH: usize = 16;

#[derive(Clone, Copy)]
struct Pending {
    n: u32,
    ingw: u32,
    ts: u32,
    v: u32,
    pc: u32,
}

#[inline(always)]
fn prefetch<T>(p: &T) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::x86_64::_mm_prefetch(p as *const T as *const i8, std::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = p;
}

pub fn parse_clicks(fx: &Fx, tl: &mut Tl, data: &'static [u8], s: usize, e: usize) {
    let (cws, we) = (fx.win.cws, fx.win.we);
    for_each_row::<4>(data, s, e, |f| {
        let cid = &data[f[0].0..f[0].1];
        let user = &data[f[1].0..f[1].1];
        if cid.is_empty() || user.is_empty() {
            return;
        }
        let ts = match parse_ts(&data[f[3].0..f[3].1]) {
            Some(t) if t >= cws && t < we => t,
            _ => return,
        };
        let camp = match fx.ctab.lookup(&data[f[2].0..f[2].1]) {
            Some(c) => c,
            None => return,
        };
        let n = if fx.ck_cap > 0 { code_of(cid, b'k').map(|c| number(c, fx.ck_key)).filter(|&n| (n as usize) < fx.ck_cap) } else { None };
        let v = user_v(fx, user);
        match (n, v) {
            (Some(n), Some(v)) => {
                let n = n as usize;
                if (v & 511) as u32 >= NALIAS {
                    flag_slow(fx, tl, (v >> 9) as usize);
                }
                if n >= tl.nmax_ck {
                    tl.nmax_ck = n + 1;
                }
                let r = &fx.ck[n];
                r.ts.store((ts - cws) as u32, Relaxed);
                r.v.store(v as u32, Relaxed);
                r.camp.store(camp + 1, Relaxed);
            }
            _ => tl.ex_ck.push(ExCk { cid, user, ts, camp }),
        }
    });
    flush_counts(fx, tl);
}

pub fn parse_links(fx: &Fx, tl: &mut Tl, data: &'static [u8], s: usize, e: usize) {
    for_each_row::<2>(data, s, e, |f| {
        let a = &data[f[0].0..f[0].1];
        let b = &data[f[1].0..f[1].1];
        if a.is_empty() || b.is_empty() {
            return;
        }
        let (va, vb) = (user_v_any(fx, a), user_v_any(fx, b));
        if let (Some(va), Some(vb)) = (va, vb) {
            let (ua, ub) = (va >> 9, vb >> 9);
            if ua == ub && ua < UCAP && ((va & 511) as u32) < NALIAS && ((vb & 511) as u32) < NALIAS {
                word_union(&fx.words[ua as usize], (va & 511) as u32, (vb & 511) as u32);
                return;
            }
        }
        for v in [va, vb].into_iter().flatten() {
            if (v >> 9) < UCAP {
                flag_slow(fx, tl, (v >> 9) as usize);
            }
        }
        tl.ex_lk.push(ExLink { a, b });
    });
    flush_counts(fx, tl);
}

/// `v` of any canonical user ID (the number may be beyond `UCAP`).
#[inline]
pub fn user_v_any(fx: &Fx, b: &[u8]) -> Option<u64> {
    code_of(b, b'u').map(|c| number(c, fx.u_key))
}

/// Publishes the extras counts and hands off when they pass their limits.
fn flush_counts(fx: &Fx, tl: &mut Tl) {
    let (a, b, c) = (tl.ex_ev.len(), tl.ex_ck.len(), tl.ex_lk.len());
    let da = a - tl.sent.0;
    let db = b - tl.sent.1;
    let dc = c - tl.sent.2;
    tl.sent = (a, b, c);
    if da > 0 && fx.ex_ev.fetch_add(da, Relaxed) + da > fx.ex_ev_limit {
        give_up("too many event rows that are not generator rows");
    }
    if db > 0 && fx.ex_ck.fetch_add(db, Relaxed) + db > fx.ex_ck_limit {
        give_up("too many click rows that are not generator rows");
    }
    if dc > 0 && fx.ex_lk.fetch_add(dc, Relaxed) + dc > fx.ex_lk_limit {
        give_up("too many link rows that are not plain same-user links");
    }
}
