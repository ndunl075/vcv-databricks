//! Per-person sessionization, attribution and aggregation (one person-shard at a time).

use crate::acc::CampAcc;
use crate::buckets::Buckets;
use crate::parquet::{MaybeTextColumn, TextColumn};
use crate::rec::*;
use crate::util::{field_at, floor_div, DAY_MS, GAP_MS, WEEK_MS};

/// user_daily rows built by one worker, column by column.
#[derive(Default)]
pub struct UserCols {
    pub user: TextColumn,
    pub day: Vec<i32>,
    pub sessions: Vec<i64>,
    pub events: Vec<i64>,
    pub revenue: Vec<i64>,
    pub top: MaybeTextColumn,
}

#[derive(Default)]
pub struct Scratch {
    starts: Vec<i64>,
    days: Vec<i32>,
    revs: Vec<u64>,
    purch: Vec<bool>,
    day_purch: Vec<(u32, u64)>,
    ck: Vec<(u32, i64)>,
    gfirst: Vec<u32>,
    gday: Vec<i32>,
}

pub struct Out {
    pub users: UserCols,
    pub acc: CampAcc,
}

/// Maps a person key to a small per-shard number; grows as persons arrive.
#[derive(Default)]
struct PersonIndex {
    slots: Vec<u32>,
    keys: Vec<(u64, u64)>,
}

impl PersonIndex {
    fn reset(&mut self) {
        self.slots.clear();
        self.slots.resize(1024, 0);
        self.keys.clear();
    }

    #[inline]
    fn find(&self, lo: u64, hi: u64) -> Option<u32> {
        let mask = self.slots.len() - 1;
        let mut i = lo as usize & mask;
        loop {
            let v = self.slots[i];
            if v == 0 {
                return None;
            }
            if self.keys[(v - 1) as usize] == (lo, hi) {
                return Some(v - 1);
            }
            i = (i + 1) & mask;
        }
    }

    #[inline]
    fn find_or_add(&mut self, lo: u64, hi: u64) -> u32 {
        let mask = self.slots.len() - 1;
        let mut i = lo as usize & mask;
        loop {
            let v = self.slots[i];
            if v == 0 {
                break;
            }
            if self.keys[(v - 1) as usize] == (lo, hi) {
                return v - 1;
            }
            i = (i + 1) & mask;
        }
        let id = self.keys.len() as u32;
        self.keys.push((lo, hi));
        self.slots[i] = id + 1;
        if self.keys.len() * 2 > self.slots.len() {
            let cap = self.slots.len() * 2;
            self.slots.clear();
            self.slots.resize(cap, 0);
            for (n, k) in self.keys.iter().enumerate() {
                let mut j = k.0 as usize & (cap - 1);
                while self.slots[j] != 0 {
                    j = (j + 1) & (cap - 1);
                }
                self.slots[j] = n as u32 + 1;
            }
        }
        id
    }
}

/// Working memory for one person shard, reused from shard to shard.
#[derive(Default)]
pub struct ShardBuf {
    index: PersonIndex,
    ev_person: Vec<u32>,
    ck_person: Vec<u32>,
    ev_at: Vec<u32>,
    ck_at: Vec<u32>,
    evs: Vec<SEv>,
    cks: Vec<SClick>,
}

fn prefix_sums(counts: &mut Vec<u32>) {
    let mut run = 0u32;
    for c in counts.iter_mut() {
        let n = *c;
        *c = run;
        run += n;
    }
    counts.push(run);
}

/// Groups the shard's events and clicks by person (counting sort), orders each person's rows by
/// time, and runs every person.
pub fn process_shard(cx: &Ctx, eb: &[Buckets<SEv>], cb: &[Buckets<SClick>], buf: &mut ShardBuf, scr: &mut Scratch, out: &mut Out) {
    let n_ev: usize = eb.iter().map(|b| b.len()).sum();
    if n_ev == 0 {
        return;
    }
    buf.index.reset();
    buf.ev_person.clear();
    buf.ev_at.clear();
    for e in eb.iter().flat_map(|b| b.blocks()).flatten() {
        let id = buf.index.find_or_add(e.lo, e.hi);
        if id as usize == buf.ev_at.len() {
            buf.ev_at.push(0);
        }
        buf.ev_at[id as usize] += 1;
        buf.ev_person.push(id);
    }
    let persons = buf.index.keys.len();
    buf.ck_at.clear();
    buf.ck_at.resize(persons, 0);
    buf.ck_person.clear();
    for c in cb.iter().flat_map(|b| b.blocks()).flatten() {
        match buf.index.find(c.lo, c.hi) {
            Some(id) => {
                buf.ck_at[id as usize] += 1;
                buf.ck_person.push(id);
            }
            None => buf.ck_person.push(u32::MAX),
        }
    }
    prefix_sums(&mut buf.ev_at);
    prefix_sums(&mut buf.ck_at);
    let (ev_end, ck_end) = (buf.ev_at[persons] as usize, buf.ck_at[persons] as usize);
    // Every slot below is written exactly once before it is read.
    buf.evs.clear();
    buf.evs.reserve(ev_end);
    buf.cks.clear();
    buf.cks.reserve(ck_end);
    let mut cursor: Vec<u32> = buf.ev_at[..persons].to_vec();
    let evs_ptr = buf.evs.as_mut_ptr();
    for (e, &id) in eb.iter().flat_map(|b| b.blocks()).flatten().zip(&buf.ev_person) {
        let at = &mut cursor[id as usize];
        unsafe { evs_ptr.add(*at as usize).write(*e) };
        *at += 1;
    }
    unsafe { buf.evs.set_len(ev_end) };
    cursor.clear();
    cursor.extend_from_slice(&buf.ck_at[..persons]);
    let cks_ptr = buf.cks.as_mut_ptr();
    for (c, &id) in cb.iter().flat_map(|b| b.blocks()).flatten().zip(&buf.ck_person) {
        if id != u32::MAX {
            let at = &mut cursor[id as usize];
            unsafe { cks_ptr.add(*at as usize).write(*c) };
            *at += 1;
        }
    }
    unsafe { buf.cks.set_len(ck_end) };

    for id in 0..persons {
        let evs = &mut buf.evs[buf.ev_at[id] as usize..buf.ev_at[id + 1] as usize];
        let cks = &mut buf.cks[buf.ck_at[id] as usize..buf.ck_at[id + 1] as usize];
        evs.sort_unstable_by_key(|e| e.ts);
        if cks.len() > 1 {
            cks.sort_unstable_by(|a, b| a.ts.cmp(&b.ts).then_with(|| unsafe { field_at(a.cid).cmp(field_at(b.cid)) }));
        }
        person(cx, evs, cks, scr, out);
    }
}

fn flush_day(cx: &Ctx, name: &[u8], day: i32, ns: i64, ne: i64, rev: i64, purch: &mut Vec<(u32, u64)>, out: &mut UserCols) {
    out.user.push(name);
    out.day.push(day);
    out.sessions.push(ns);
    out.events.push(ne);
    out.revenue.push(rev);
    if purch.is_empty() {
        out.top.push(None);
    } else {
        purch.sort_unstable_by_key(|p| p.0);
        let mut best_cat = 0u32;
        let mut best_sum = 0u64;
        let mut k = 0;
        while k < purch.len() {
            let c = purch[k].0;
            let mut sum = 0u64;
            while k < purch.len() && purch[k].0 == c {
                sum += purch[k].1;
                k += 1;
            }
            if sum > best_sum {
                best_sum = sum;
                best_cat = c;
            }
        }
        out.top.push(Some(cx.prods.cats[best_cat as usize]));
        purch.clear();
    }
}

fn person(cx: &Ctx, evs: &[SEv], clicks: &[SClick], scr: &mut Scratch, out: &mut Out) {
    let name = cx.name(evs[0].name);
    scr.starts.clear();
    scr.days.clear();
    scr.revs.clear();
    scr.purch.clear();
    scr.day_purch.clear();

    let mut cur_day = i32::MIN;
    let (mut d_s, mut d_e, mut d_r) = (0i64, 0i64, 0i64);
    let mut s_rev = 0u64;
    let mut s_has = false;
    let mut last = 0i64;
    let mut started = false;
    for e in evs {
        if !started || e.ts - last > GAP_MS {
            if started {
                scr.revs.push(s_rev);
                scr.purch.push(s_has);
            }
            let day = floor_div(e.ts, DAY_MS) as i32;
            if day != cur_day {
                if cur_day != i32::MIN {
                    flush_day(cx, name, cur_day, d_s, d_e, d_r, &mut scr.day_purch, &mut out.users);
                }
                cur_day = day;
                d_s = 0;
                d_e = 0;
                d_r = 0;
            }
            d_s += 1;
            scr.starts.push(e.ts);
            scr.days.push(day);
            s_rev = 0;
            s_has = false;
            started = true;
        }
        last = e.ts;
        d_e += 1;
        if e.price > 0 {
            s_rev += e.price as u64;
            s_has = true;
            d_r += e.price as i64;
            scr.day_purch.push((e.cat, e.price as u64));
        }
    }
    scr.revs.push(s_rev);
    scr.purch.push(s_has);
    flush_day(cx, name, cur_day, d_s, d_e, d_r, &mut scr.day_purch, &mut out.users);

    let organic = cx.camps.organic();
    let ns = scr.starts.len();
    if clicks.is_empty() {
        for s in 0..ns {
            out.acc.credit(organic, scr.days[s], scr.purch[s], scr.revs[s]);
        }
        return;
    }
    // Credit: the latest click at or before the start, within 7 days; ties: smallest click_id.
    for s in 0..ns {
        let st = scr.starts[s];
        let n = clicks.partition_point(|c| c.ts <= st);
        let camp = if n == 0 || st - clicks[n - 1].ts > WEEK_MS {
            organic
        } else {
            let t = clicks[n - 1].ts;
            let first = clicks[..n].partition_point(|c| c.ts < t);
            clicks[first].camp
        };
        out.acc.credit(camp, scr.days[s], scr.purch[s], scr.revs[s]);
    }
    // Assists: per campaign, merge the 7-day reaches of its clicks and count sessions inside.
    scr.gfirst.clear();
    scr.gday.clear();
    for s in 0..ns {
        if s == 0 || scr.days[s] != scr.days[s - 1] {
            scr.gfirst.push(s as u32);
            scr.gday.push(scr.days[s]);
        }
    }
    scr.ck.clear();
    scr.ck.extend(clicks.iter().map(|c| (c.camp, c.ts)));
    scr.ck.sort_unstable();
    let mut k = 0;
    while k < scr.ck.len() {
        let camp = scr.ck[k].0;
        let mut a = scr.ck[k].1;
        let mut b = a + WEEK_MS;
        k += 1;
        loop {
            let more = k < scr.ck.len() && scr.ck[k].0 == camp;
            if more && scr.ck[k].1 <= b {
                b = scr.ck[k].1 + WEEK_MS;
                k += 1;
                continue;
            }
            assist_range(scr, &mut out.acc, camp, a, b);
            if !more {
                break;
            }
            a = scr.ck[k].1;
            b = a + WEEK_MS;
            k += 1;
        }
    }
}

/// Counts the sessions with start in [a, b] per day for one campaign.
fn assist_range(scr: &Scratch, acc: &mut CampAcc, camp: u32, a: i64, b: i64) {
    let lo = scr.starts.partition_point(|&s| s < a);
    let hi = scr.starts.partition_point(|&s| s <= b);
    if lo >= hi {
        return;
    }
    let mut g = scr.gfirst.partition_point(|&f| (f as usize) <= lo) - 1;
    while g < scr.gfirst.len() && (scr.gfirst[g] as usize) < hi {
        let gs = (scr.gfirst[g] as usize).max(lo);
        let ge = if g + 1 < scr.gfirst.len() { (scr.gfirst[g + 1] as usize).min(hi) } else { hi };
        acc.assist(camp, scr.gday[g], (ge - gs) as u64);
        g += 1;
    }
}
