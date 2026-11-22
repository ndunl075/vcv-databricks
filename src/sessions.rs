//! One shard at a time: group rows by person, order each person's rows by time, cut sessions,
//! credit and assist campaigns, and append the person's `user_daily` rows.

use crate::acc::CampAcc;
use crate::ids::Names;
use crate::parquet::{MaybeTextColumn, TextColumn};
use crate::parse::{Ck, Run};
use crate::people::People;
use crate::shard::{Row, Seating};
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

pub struct Out {
    pub users: UserCols,
    pub acc: CampAcc,
}

/// Dealt rows of one kind: for each thread, where each shard's rows start and how many there are.
pub struct Dealt<'a> {
    pub rows: *const Row,
    pub at: &'a [Vec<usize>],
    pub got: &'a [Vec<u32>],
}

impl Dealt<'_> {
    fn parts(&self, shard: usize) -> impl Iterator<Item = &[Row]> + Clone + '_ {
        // SAFETY: `at` and `got` describe ranges that were filled inside `rows`.
        self.at.iter().zip(self.got).map(move |(a, g)| unsafe { std::slice::from_raw_parts(self.rows.add(a[shard]), g[shard] as usize) })
    }
    pub fn count(&self, shard: usize) -> usize {
        self.got.iter().map(|g| g[shard] as usize).sum()
    }
}

/// Read-only inputs of the person stage.
pub struct Stage<'a> {
    pub win: &'a Window,
    pub prods: &'a Products,
    pub camps: &'a Campaigns,
    pub names: &'a Names,
    pub people: &'a People,
    pub seats: &'a Seating,
    pub events: Dealt<'a>,
    pub clicks: Dealt<'a>,
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
    ev_end: Vec<u32>,
    ck_end: Vec<u32>,
    evs: Vec<Row>,
    cks: Vec<Row>,
    spare: Vec<Row>,
    starts: Vec<i64>,
    days: Vec<i32>,
    revs: Vec<u64>,
    bought: Vec<bool>,
    day_buys: Vec<(u32, u64)>,
    camp_times: Vec<(u32, i64)>,
    day_first: Vec<u32>,
    day_of: Vec<i32>,
}

/// Places `parts` into `out` grouped by `local(row)`; afterwards `end[l]` is one past the last
/// row of person `l`.
fn group(parts: impl Iterator<Item = impl AsRef<[Row]>> + Clone, people: usize, local: impl Fn(&Row) -> usize, end: &mut Vec<u32>, out: &mut Vec<Row>) {
    end.clear();
    end.resize(people + 1, 0);
    let mut total = 0usize;
    for part in parts.clone() {
        for r in part.as_ref() {
            end[local(r) + 1] += 1;
        }
        total += part.as_ref().len();
    }
    let mut run = 0u32;
    for e in end.iter_mut() {
        run += *e;
        *e = run;
    }
    out.clear();
    out.reserve(total);
    let base = out.as_mut_ptr();
    for part in parts {
        for r in part.as_ref() {
            let slot = &mut end[local(r)];
            // SAFETY: slots are distinct and below `total`; every slot is written once.
            unsafe { base.add(*slot as usize).write(*r) };
            *slot += 1;
        }
    }
    // SAFETY: all `total` slots were written.
    unsafe { out.set_len(total) };
}

/// Orders rows by their first word.
fn by_first_word(rows: &mut [Row], spare: &mut Vec<Row>) {
    let n = rows.len();
    if n <= 24 {
        for i in 1..n {
            let x = rows[i];
            let mut j = i;
            while j > 0 && rows[j - 1][0] > x[0] {
                rows[j] = rows[j - 1];
                j -= 1;
            }
            rows[j] = x;
        }
    } else if n < 4096 {
        rows.sort_unstable_by_key(|r| r[0]);
    } else {
        by_digits(rows, spare);
    }
}

/// Stable sort by the first word, 11 bits per pass, skipping the bits no row differs in.
fn by_digits(rows: &mut [Row], spare: &mut Vec<Row>) {
    let (mut all_or, mut all_and) = (0u64, !0u64);
    for r in rows.iter() {
        all_or |= r[0];
        all_and &= r[0];
    }
    let varying = all_or ^ all_and;
    if varying == 0 {
        return;
    }
    let low = varying.trailing_zeros();
    let span = 64 - varying.leading_zeros() - low;
    let passes = span.div_ceil(11);
    spare.clear();
    spare.resize(rows.len(), [0; 2]);
    let mut count = vec![0u32; 2048];
    let mut in_rows = true;
    for p in 0..passes {
        let shift = low + p * 11;
        count.iter_mut().for_each(|c| *c = 0);
        let (src, dst): (&[Row], &mut [Row]) = if in_rows { (&*rows, &mut spare[..]) } else { (&spare[..], &mut *rows) };
        for r in src {
            count[((r[0] >> shift) & 2047) as usize] += 1;
        }
        let mut run = 0u32;
        for c in count.iter_mut() {
            let n = *c;
            *c = run;
            run += n;
        }
        for r in src {
            let c = &mut count[((r[0] >> shift) & 2047) as usize];
            dst[*c as usize] = *r;
            *c += 1;
        }
        in_rows = !in_rows;
    }
    if !in_rows {
        rows.copy_from_slice(spare);
    }
}

/// Runs every person of one shard.
pub fn shard(st: &Stage, shard: usize, b: &mut Bench, out: &mut Out) {
    if st.events.count(shard) == 0 {
        return;
    }
    let bits = st.seats.local_bits;
    let people = 1usize << bits;
    let low = (people - 1) as u64;
    group(st.events.parts(shard), people, |r| (r[1] & low) as usize, &mut b.ev_end, &mut b.evs);
    group(st.clicks.parts(shard), people, |r| (r[0] & low) as usize, &mut b.ck_end, &mut b.cks);
    let mut evs = std::mem::take(&mut b.evs);
    let mut cks = std::mem::take(&mut b.cks);
    let (mut e0, mut c0) = (0usize, 0usize);
    for l in 0..people {
        let (e1, c1) = (b.ev_end[l] as usize, b.ck_end[l] as usize);
        if e1 > e0 {
            by_first_word(&mut evs[e0..e1], &mut b.spare);
            if c1 - c0 > 1 {
                by_first_word(&mut cks[c0..c1], &mut b.spare);
            }
            let seat = ((shard as u32) << bits) | l as u32;
            person(st, seat, &evs[e0..e1], &cks[c0..c1], b, out);
        }
        e0 = e1;
        c0 = c1;
    }
    b.evs = evs;
    b.cks = cks;
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

/// Time of a dealt click, on the events' clock (milliseconds after the window start).
#[inline(always)]
fn click_time(c: &Row) -> i64 {
    (c[0] >> 24) as i64 - WEEK_MS
}

#[inline(always)]
fn click_camp(c: &Row) -> u32 {
    c[1] as u32
}

/// Campaign of the click that credits a session, given that `clicks[..upto]` are the clicks at
/// or before the session start and the last of them is within seven days.
#[inline]
fn credited(st: &Stage, clicks: &[Row], upto: usize) -> u32 {
    let last = &clicks[upto - 1];
    let t = click_time(last);
    if upto < 2 || click_time(&clicks[upto - 2]) != t {
        return click_camp(last);
    }
    // Several clicks at the same millisecond: the smallest click_id wins.
    let mut first = upto - 1;
    let mut one_campaign = true;
    while first > 0 && click_time(&clicks[first - 1]) == t {
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

fn person(st: &Stage, seat: u32, evs: &[Row], clicks: &[Row], b: &mut Bench, out: &mut Out) {
    let lead = st.seats.lead(seat);
    // SAFETY: `named` holds a number that was handed out.
    let name = unsafe { (*st.names.text(st.people.named[lead as usize])).bytes() };
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
        let t = (e[0] >> 24) as i64;
        let price = e[0] & 0xff_ffff;
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
        while upto < nc && click_time(&clicks[upto]) <= start {
            upto += 1;
        }
        let camp = if upto == 0 || start - click_time(&clicks[upto - 1]) > WEEK_MS { organic } else { credited(st, clicks, upto) };
        out.acc.credit(camp, b.days[s], b.bought[s], b.revs[s]);
        if small {
            while from < upto && start - click_time(&clicks[from]) > WEEK_MS {
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
    b.camp_times.extend(clicks.iter().map(|c| (click_camp(c), click_time(c))));
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
        for (n, mask) in [(5000usize, !0u64), (5000, 0xffff_ff00_0000), (9000, 0x7ff), (4096, 0), (6000, 0xffff_ffff_ff00_0000)] {
            let mut rows: Vec<Row> = (0..n).map(|i| [next() & mask, i as u64]).collect();
            let mut want = rows.clone();
            want.sort_by_key(|r| r[0]);
            let mut spare = Vec::new();
            by_first_word(&mut rows, &mut spare);
            assert_eq!(rows, want);
        }
        let mut small: Vec<Row> = (0..20).map(|i| [next() % 7, i]).collect();
        let mut want = small.clone();
        want.sort_by_key(|r| r[0]);
        by_first_word(&mut small, &mut Vec::new());
        assert_eq!(small.iter().map(|r| r[0]).collect::<Vec<_>>(), want.iter().map(|r| r[0]).collect::<Vec<_>>());
    }
}
