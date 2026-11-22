//! One shard at a time: group rows by person, order each person's rows by time, cut sessions,
//! credit and assist campaigns, and append the person's `user_daily` rows.

use crate::acc::CampAcc;
use crate::ids::Names;
use crate::os;
use crate::parquet::{MaybeTextColumn, TextColumn};
use crate::parse::{Ck, Run};
use crate::people::People;
use crate::shard::{Dealt, Row, Seating};
use crate::tables::{Campaigns, Products, Window};
use crate::util::{field_at, DAY_MS, GAP_MS, WEEK_MS};

/// user_daily rows built by one worker, column by column.
#[derive(Default)]
pub struct UserCols {
    pub user: TextColumn,
    pub day: Vec<i32>,
    pub sessions: Vec<i32>,
    pub events: Vec<i32>,
    pub revenue: Vec<i64>,
    pub top: MaybeTextColumn,
}

impl UserCols {
    /// Columns with room for about `rows` rows before anything has to move.
    pub fn with_room(rows: usize) -> UserCols {
        UserCols {
            user: TextColumn::with_room(rows, 20),
            day: Vec::with_capacity(rows),
            sessions: Vec::with_capacity(rows),
            events: Vec::with_capacity(rows),
            revenue: Vec::with_capacity(rows),
            top: MaybeTextColumn::with_room(rows, 4),
        }
    }
}

pub struct Out {
    pub users: UserCols,
    pub acc: CampAcc,
}

/// Read-only inputs of the person stage.
pub struct Stage<'a> {
    pub win: &'a Window,
    pub prods: &'a Products,
    pub camps: &'a Campaigns,
    pub names: &'a Names,
    pub people: &'a People,
    pub seats: &'a Seating,
    pub events: &'a Dealt,
    pub clicks: &'a Dealt,
    /// Low bits of a dealt row's first word that hold its time.
    pub event_time_bits: u32,
    pub click_time_bits: u32,
    /// All click runs with the number of each run's first record, ascending.
    pub click_runs: &'a [(Run<Ck>, u32)],
}

// SAFETY: everything behind the raw pointers is read-only in this stage.
unsafe impl Sync for Stage<'_> {}

impl Stage<'_> {
    /// The click_id of click record `number`.
    fn click_id(&self, number: u32) -> &[u8] {
        let i = self.click_runs.partition_point(|(_, first)| *first <= number) - 1;
        let (run, first) = &self.click_runs[i];
        // SAFETY: the record exists and its click_id is followed by a delimiter in its row.
        unsafe { field_at((*run.first.add((number - first) as usize)).id_at as u64) }
    }
}

/// Working memory of one worker.
#[derive(Default)]
pub struct Bench {
    ev: [Vec<Row>; 2],
    ck: [Vec<Row>; 2],
    counts: Vec<u32>,
    /// (person-in-shard, first event, one past the last event) for every person of the shard.
    spans: Vec<(u32, u32, u32)>,
    starts: Vec<i64>,
    days: Vec<i32>,
    revs: Vec<u64>,
    bought: Vec<bool>,
    day_buys: Vec<(u32, u64)>,
    camp_times: Vec<(u32, i64)>,
    day_first: Vec<u32>,
    day_of: Vec<i32>,
}

const DIGIT: u32 = 11;
const BUCKETS: usize = 1 << DIGIT;

/// Sorts the rows of `parts` by the low `bits` bits of their first word (the higher bits must be
/// equal). The sorted rows end up in `bufs[i]` for the returned `i`.
fn sorted(parts: impl Iterator<Item = impl AsRef<[Row]>> + Clone, bits: u32, bufs: &mut [Vec<Row>; 2], counts: &mut Vec<u32>) -> usize {
    let total: usize = parts.clone().map(|p| p.as_ref().len()).sum();
    for b in bufs.iter_mut() {
        b.clear();
        b.reserve(total);
    }
    if total < 192 {
        for p in parts {
            bufs[0].extend_from_slice(p.as_ref());
        }
        bufs[0].sort_unstable_by_key(|r| r[0]);
        return 0;
    }
    let passes = bits.div_ceil(DIGIT).max(1) as usize;
    counts.clear();
    counts.resize(passes * BUCKETS, 0);
    for part in parts.clone() {
        for r in part.as_ref() {
            let mut w = r[0];
            for p in 0..passes {
                // SAFETY: the index is below passes * BUCKETS.
                unsafe { *counts.get_unchecked_mut(p * BUCKETS + (w as usize & (BUCKETS - 1))) += 1 };
                w >>= DIGIT;
            }
        }
    }
    // SAFETY: both buffers have room for `total` rows; every pass writes each slot exactly once
    // before the next pass (or the caller) reads it.
    unsafe {
        bufs[0].set_len(total);
        bufs[1].set_len(total);
    }
    let mut from_parts = true;
    let mut cur = 1usize; // rows are in bufs[cur] once `from_parts` is false
    for p in 0..passes {
        let c = &mut counts[p * BUCKETS..(p + 1) * BUCKETS];
        if c.iter().any(|&n| n as usize == total) {
            continue; // every row has the same digit here
        }
        let mut run = 0u32;
        for x in c.iter_mut() {
            let n = *x;
            *x = run;
            run += n;
        }
        let shift = p as u32 * DIGIT;
        let dst = 1 - cur;
        let out = bufs[dst].as_mut_ptr();
        let mut place = |r: &Row| {
            // SAFETY: counts place every row at a distinct index below `total`.
            unsafe {
                let slot = c.get_unchecked_mut((r[0] >> shift) as usize & (BUCKETS - 1));
                out.add(*slot as usize).write(*r);
                *slot += 1;
            }
        };
        if from_parts {
            for part in parts.clone() {
                part.as_ref().iter().for_each(&mut place);
            }
            from_parts = false;
        } else {
            let (src_ptr, n) = (bufs[cur].as_ptr(), total);
            for i in 0..n {
                // SAFETY: `i < total`, and source and destination are different buffers.
                place(unsafe { &*src_ptr.add(i) });
            }
        }
        cur = dst;
    }
    if from_parts {
        let mut at = 0usize;
        for part in parts {
            let part = part.as_ref();
            bufs[0][at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        return 0;
    }
    cur
}

/// Runs every person of one shard.
pub fn shard(st: &Stage, shard: usize, b: &mut Bench, out: &mut Out) {
    if st.events.count(shard) == 0 {
        return;
    }
    let bits = st.seats.local_bits;
    let (etb, ctb) = (st.event_time_bits, st.click_time_bits);
    let ei = sorted(st.events.parts(shard), bits + etb, &mut b.ev, &mut b.counts);
    let ci = sorted(st.clicks.parts(shard), bits + ctb, &mut b.ck, &mut b.counts);
    let evs = std::mem::take(&mut b.ev[ei]);
    let cks = std::mem::take(&mut b.ck[ci]);
    let mut spans = std::mem::take(&mut b.spans);
    spans.clear();
    let mut i = 0usize;
    while i < evs.len() {
        let who = evs[i][0] >> etb;
        let mut j = i + 1;
        while j < evs.len() && evs[j][0] >> etb == who {
            j += 1;
        }
        spans.push((who as u32, i as u32, j as u32));
        i = j;
    }
    // Names live far apart in memory; ask for them a few people ahead.
    let lead_of = |who: u32| st.seats.lead(((shard as u32) << bits) | who) as usize;
    let mut c = 0usize;
    for (k, &(who, e0, e1)) in spans.iter().enumerate() {
        if let Some(&(ahead, _, _)) = spans.get(k + 16) {
            os::soon(&st.people.named[lead_of(ahead)]);
        }
        if let Some(&(ahead, _, _)) = spans.get(k + 8) {
            os::soon(st.names.text(st.people.named[lead_of(ahead)]));
        }
        while c < cks.len() && ((cks[c][0] >> ctb) as u32) < who {
            c += 1;
        }
        let c0 = c;
        while c < cks.len() && (cks[c][0] >> ctb) as u32 == who {
            c += 1;
        }
        person(st, lead_of(who), &evs[e0 as usize..e1 as usize], &cks[c0..c], b, out);
    }
    b.ev[ei] = evs;
    b.ck[ci] = cks;
    b.spans = spans;
}

fn close_day(st: &Stage, name: &[u8], day: i32, sessions: i32, events: i32, revenue: i64, buys: &mut Vec<(u32, u64)>, out: &mut UserCols) {
    out.user.push(name);
    out.day.push(day);
    out.sessions.push(sessions);
    out.events.push(events);
    out.revenue.push(revenue);
    match buys.len() {
        0 => out.top.push(None),
        1 => out.top.push(Some(st.prods.cats[buys[0].0 as usize])),
        _ => {
            // Categories are numbered in byte order, so the first best sum is the tie winner.
            buys.sort_unstable_by_key(|p| p.0);
            let (mut best_cat, mut best_sum, mut k) = (0u32, 0u64, 0usize);
            while k < buys.len() {
                let cat = buys[k].0;
                let mut sum = 0u64;
                while k < buys.len() && buys[k].0 == cat {
                    sum += buys[k].1;
                    k += 1;
                }
                if sum > best_sum {
                    best_sum = sum;
                    best_cat = cat;
                }
            }
            out.top.push(Some(st.prods.cats[best_cat as usize]));
        }
    }
    buys.clear();
}

/// Time of a dealt click with its person bits removed, on the events' clock (milliseconds
/// after the window start). `mask` keeps the time bits.
#[inline(always)]
fn click_time(c: &Row, mask: u64) -> i64 {
    (c[0] & mask) as i64 - WEEK_MS
}

#[inline(always)]
fn click_camp(c: &Row) -> u32 {
    c[1] as u32
}

/// Campaign of the click that credits a session, given that `clicks[..upto]` are the clicks at
/// or before the session start and the last of them is within seven days.
#[inline]
fn credited(st: &Stage, clicks: &[Row], upto: usize, m: u64) -> u32 {
    let last = &clicks[upto - 1];
    let t = click_time(last, m);
    if upto < 2 || click_time(&clicks[upto - 2], m) != t {
        return click_camp(last);
    }
    // Several clicks at the same millisecond: the smallest click_id wins.
    let mut first = upto - 1;
    let mut one_campaign = true;
    while first > 0 && click_time(&clicks[first - 1], m) == t {
        first -= 1;
        one_campaign &= click_camp(&clicks[first]) == click_camp(last);
    }
    if one_campaign {
        return click_camp(last);
    }
    let id = |c: &Row| st.click_id((c[1] >> 32) as u32);
    let mut best = last;
    for c in &clicks[first..upto - 1] {
        if id(c) < id(best) {
            best = c;
        }
    }
    click_camp(best)
}

fn person(st: &Stage, lead: usize, evs: &[Row], clicks: &[Row], b: &mut Bench, out: &mut Out) {
    // SAFETY: `named` holds a number that was handed out.
    let name = unsafe { (*st.names.text(st.people.named[lead])).bytes() };
    let time_mask = (1u64 << st.event_time_bits) - 1;
    let m = (1u64 << st.click_time_bits) - 1;
    b.starts.clear();
    b.days.clear();
    b.revs.clear();
    b.bought.clear();
    b.day_buys.clear();

    let first_day = st.win.ws.div_euclid(DAY_MS);
    let into_day = st.win.ws.rem_euclid(DAY_MS);
    let mut cur_day = i32::MIN;
    let (mut d_sessions, mut d_events, mut d_revenue) = (0i32, 0i32, 0i64);
    let (mut s_revenue, mut s_bought) = (0u64, false);
    let mut last = i64::MIN / 2;
    for e in evs {
        let t = (e[0] & time_mask) as i64;
        let price = e[1] & 0xffff_ffff;
        if t - last > GAP_MS {
            if !b.starts.is_empty() {
                b.revs.push(s_revenue);
                b.bought.push(s_bought);
            }
            let day = (first_day + (into_day + t) / DAY_MS) as i32;
            if day != cur_day {
                if cur_day != i32::MIN {
                    close_day(st, name, cur_day, d_sessions, d_events, d_revenue, &mut b.day_buys, &mut out.users);
                }
                cur_day = day;
                d_sessions = 0;
                d_events = 0;
                d_revenue = 0;
            }
            d_sessions += 1;
            b.starts.push(t);
            b.days.push(day);
            s_revenue = 0;
            s_bought = false;
        }
        last = t;
        d_events += 1;
        if price > 0 {
            s_revenue += price;
            s_bought = true;
            d_revenue += price as i64;
            b.day_buys.push(((e[1] >> 32) as u32, price));
        }
    }
    b.revs.push(s_revenue);
    b.bought.push(s_bought);
    close_day(st, name, cur_day, d_sessions, d_events, d_revenue, &mut b.day_buys, &mut out.users);

    let organic = st.camps.organic();
    let ns = b.starts.len();
    if clicks.is_empty() {
        for s in 0..ns {
            out.acc.credit(organic, b.days[s], b.bought[s], b.revs[s]);
        }
        return;
    }
    let nc = clicks.len();
    let small = nc <= 16;
    // `upto`: clicks at or before the start. `from`: first click inside the seven days.
    let (mut upto, mut from) = (0usize, 0usize);
    for s in 0..ns {
        let start = b.starts[s];
        while upto < nc && click_time(&clicks[upto], m) <= start {
            upto += 1;
        }
        let camp = if upto == 0 || start - click_time(&clicks[upto - 1], m) > WEEK_MS { organic } else { credited(st, clicks, upto, m) };
        out.acc.credit(camp, b.days[s], b.bought[s], b.revs[s]);
        if small {
            while from < upto && start - click_time(&clicks[from], m) > WEEK_MS {
                from += 1;
            }
            let mut seen = [0u32; 16];
            let mut n = 0usize;
            for c in &clicks[from..upto] {
                let camp = click_camp(c);
                if !seen[..n].contains(&camp) {
                    seen[n] = camp;
                    n += 1;
                    out.acc.assist(camp, b.days[s], 1);
                }
            }
        }
    }
    if small {
        return;
    }
    // Many clicks: per campaign, merge the seven-day reaches of its clicks into stretches and
    // count the sessions that start inside each stretch, day by day.
    b.day_first.clear();
    b.day_of.clear();
    for s in 0..ns {
        if s == 0 || b.days[s] != b.days[s - 1] {
            b.day_first.push(s as u32);
            b.day_of.push(b.days[s]);
        }
    }
    b.camp_times.clear();
    b.camp_times.extend(clicks.iter().map(|c| (click_camp(c), click_time(c, m))));
    b.camp_times.sort_unstable();
    let mut k = 0;
    while k < b.camp_times.len() {
        let camp = b.camp_times[k].0;
        let mut a = b.camp_times[k].1;
        let mut z = a + WEEK_MS;
        k += 1;
        loop {
            let more = k < b.camp_times.len() && b.camp_times[k].0 == camp;
            if more && b.camp_times[k].1 <= z {
                z = b.camp_times[k].1 + WEEK_MS;
                k += 1;
                continue;
            }
            count_stretch(b, &mut out.acc, camp, a, z);
            if !more {
                break;
            }
            a = b.camp_times[k].1;
            z = a + WEEK_MS;
            k += 1;
        }
    }
}

/// Adds, per day, the sessions that start in `[a, z]` to the campaign's assists.
fn count_stretch(b: &Bench, acc: &mut CampAcc, camp: u32, a: i64, z: i64) {
    let lo = b.starts.partition_point(|&s| s < a);
    let hi = b.starts.partition_point(|&s| s <= z);
    if lo >= hi {
        return;
    }
    let mut g = b.day_first.partition_point(|&f| (f as usize) <= lo) - 1;
    while g < b.day_first.len() && (b.day_first[g] as usize) < hi {
        let from = (b.day_first[g] as usize).max(lo);
        let to = if g + 1 < b.day_first.len() { (b.day_first[g + 1] as usize).min(hi) } else { hi };
        acc.assist(camp, b.day_of[g], (to - from) as u64);
        g += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_sort_matches_std() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for (n, bits) in [(5000usize, 64u32), (5000, 40), (9000, 11), (4096, 1), (6000, 23), (100, 40), (0, 40), (1, 3), (300, 0)] {
            let mask = if bits == 64 { !0 } else { (1u64 << bits) - 1 };
            let rows: Vec<Row> = (0..n).map(|i| [next() & mask, i as u64]).collect();
            let mut want = rows.clone();
            want.sort_by_key(|r| r[0]);
            for cut in [1usize, 3, 7] {
                let parts: Vec<&[Row]> = rows.chunks((n / cut).max(1)).collect();
                let mut bufs = [Vec::new(), Vec::new()];
                let i = sorted(parts.iter().copied(), bits, &mut bufs, &mut Vec::new());
                assert_eq!(bufs[i].iter().map(|r| r[0]).collect::<Vec<_>>(), want.iter().map(|r| r[0]).collect::<Vec<_>>(), "n {n} bits {bits}");
                let mut seen: Vec<u64> = bufs[i].iter().map(|r| r[1]).collect();
                seen.sort_unstable();
                assert_eq!(seen, (0..n as u64).collect::<Vec<_>>());
            }
        }
    }
}
