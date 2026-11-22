//! One person's sessions, credit, assists and the output rows; plus the per-thread output.

use crate::acc::CampAcc;
use crate::handoff;
use crate::parquet::{write_file, Col, OptStrCol, StrCol};
use crate::util::{floor_div, DAY_MS, GAP_MS, WEEK_MS};
use std::path::PathBuf;

#[derive(Clone, Copy)]
pub struct E {
    pub ts: i64,
    pub price: u32,
    pub cat: u32,
}

#[derive(Clone, Copy)]
pub struct C {
    pub ts: i64,
    pub camp: u32,
}

#[derive(Default)]
pub struct UserCols {
    pub user: StrCol,
    pub day: Vec<i32>,
    pub sessions: Vec<i32>,
    pub events: Vec<i32>,
    pub revenue: Vec<i64>,
    pub top: OptStrCol,
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
    pub worker: usize,
    pub seq: usize,
    pub dir: PathBuf,
    pub rows: u64,
    pub organic: u32,
    pub cats: &'static [&'static [u8]],
}

pub const FLUSH_ROWS: usize = 1 << 20;

impl Out {
    /// Writes the buffered user rows to a new part file.
    pub fn flush(&mut self) {
        if self.users.day.is_empty() {
            return;
        }
        let path = self.dir.join(format!("ssf-{:02}-{:03}.parquet", self.worker, self.seq));
        self.seq += 1;
        if !handoff::register_output(&path) {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        let u = &self.users;
        let small = u.revenue.iter().all(|&r| r <= i32::MAX as i64);
        let rev32: Vec<i32>;
        let rev_col = if small {
            rev32 = u.revenue.iter().map(|&r| r as i32).collect();
            Col::I32(&rev32)
        } else {
            Col::I64(&u.revenue)
        };
        let r = write_file(
            &path,
            &["user_id", "day", "sessions", "events", "revenue_cents", "top_category"],
            &[Col::Str(&u.user), Col::Date(&u.day), Col::I32(&u.sessions), Col::I32(&u.events), rev_col, Col::OptStr(&u.top)],
        );
        if let Err(e) = r {
            handoff::give_up(&format!("write {}: {e}", path.display()));
        }
        self.rows += self.users.day.len() as u64;
        self.users = UserCols::default();
    }
}

fn flush_day(cats: &[&[u8]], name: &[u8], day: i32, ns: i64, ne: i64, rev: i64, purch: &mut Vec<(u32, u64)>, out: &mut UserCols) {
    out.user.push(name);
    out.day.push(day);
    out.sessions.push(ns as i32);
    out.events.push(ne as i32);
    out.revenue.push(rev);
    if purch.is_empty() {
        out.top.push(None);
    } else {
        if purch.len() > 1 {
            purch.sort_unstable_by_key(|p| p.0);
        }
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
        out.top.push(Some(cats[best_cat as usize]));
        purch.clear();
    }
}

/// `evs` sorted by `ts`; `clicks` sorted by `(ts, click_id)`.
pub fn person(name: &[u8], evs: &[E], clicks: &[C], scr: &mut Scratch, out: &mut Out) {
    scr.starts.clear();
    scr.days.clear();
    scr.revs.clear();
    scr.purch.clear();
    scr.day_purch.clear();

    let cats = out.cats;
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
                    flush_day(cats, name, cur_day, d_s, d_e, d_r, &mut scr.day_purch, &mut out.users);
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
    flush_day(cats, name, cur_day, d_s, d_e, d_r, &mut scr.day_purch, &mut out.users);

    let organic = out.organic;
    let ns = scr.starts.len();
    if clicks.is_empty() {
        for s in 0..ns {
            out.acc.credit(organic, scr.days[s], scr.purch[s], scr.revs[s]);
        }
        return;
    }
    // Credit: the latest click at or before the start, within 7 days; ties: smallest click_id
    // (the clicks are sorted by (ts, id), so the first of the latest timestamp).
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
