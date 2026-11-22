//! ssf: Shopping Sessions, fast path for generator-shaped data, with a clean hand-off to the
//! general pipeline (`/ss`) whenever a precondition fails. See fast/README.md.

mod acc;
mod csv;
mod fastuser;
mod fx;
mod handoff;
mod idc;
mod input;
mod mem;
mod parquet;
mod parse;
mod person;
mod scan;
mod slow;
mod tables;
mod ts;
mod util;

use acc::CampAcc;
use fx::*;
use handoff::give_up;
use input::{csv_files, header_end, make_tasks, map_file};
use parquet::{Col, StrCol};
use person::{Out, Scratch, UserCols};
use scan::Run;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Barrier, Mutex, OnceLock};
use std::time::Instant;
use util::{floor_div, DAY_MS};

extern "C" {
    fn _exit(code: i32) -> !;
}

macro_rules! lg {
    ($t0:expr, $($arg:tt)*) => {
        if std::env::var_os("SSF_LOG").is_some() {
            eprintln!("[{:7.3}s] {}", $t0.elapsed().as_secs_f64(), format!($($arg)*))
        }
    };
}

fn par<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    std::thread::scope(|s| {
        let hs: Vec<_> = (0..n)
            .map(|i| {
                let f = &f;
                s.spawn(move || {
                    handoff::thread_setup();
                    f(i)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn ceil_log2(x: u64) -> u32 {
    64 - x.saturating_sub(1).leading_zeros()
}

/// Lines from the middle of the largest file, for key recovery.
fn sample_lines(files: &[&'static [u8]], max_lines: usize) -> Vec<&'static [u8]> {
    let Some(big) = files.iter().max_by_key(|f| f.len()) else { return Vec::new() };
    let h = header_end(big);
    let mut p = if big.len() > (4 << 20) { big.len() / 3 } else { h };
    if p > h {
        p = match util::find_byte(big, p, b'\n') {
            Some(i) => i + 1,
            None => h,
        };
    }
    let mut v = Vec::new();
    while p < big.len() && v.len() < max_lines {
        let e = util::find_byte(big, p, b'\n').unwrap_or(big.len());
        v.push(&big[p..e]);
        p = e + 1;
    }
    v
}

fn run() -> Result<(), String> {
    let t0 = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_path = |name: &str, default: &str| PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.to_string()));
    let (data_dir, out_dir) = if args.len() >= 2 { (PathBuf::from(&args[0]), PathBuf::from(&args[1])) } else { (env_path("DATA_DIR", "/data"), env_path("OUTPUT_DIR", "/output")) };
    handoff::init(&out_dir);
    handoff::install();
    let threads = std::env::var("NCPU")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .min(256);

    // ---- Early checks: side tables and shapes. ----
    let win = tables::read_manifest(&data_dir)?;
    if win.we <= win.ws || win.we - win.ws > 40 * DAY_MS || win.ws < 8 * DAY_MS {
        return Err("window outside the supported range".into());
    }
    let prods = tables::read_products(&data_dir)?;
    if prods.cats.len() >= 250 {
        return Err("too many categories".into());
    }
    let camps = tables::read_campaigns(&data_dir)?;
    let ctab = CampTab::build(&camps).ok_or("campaign ids are not c_<number>")?;
    let io = |e: std::io::Error| e.to_string();
    let map_all = |sub: &str| -> Result<Vec<&'static [u8]>, String> {
        let mut v = Vec::new();
        for p in csv_files(&data_dir.join(sub)).map_err(io)? {
            v.push(map_file(&p).map_err(|e| format!("{}: {e}", p.display()))?);
        }
        Ok(v)
    };
    let ev_files = map_all("events")?;
    let ck_files = map_all("clicks")?;
    let lk_files = map_all("links")?;
    let ev_bytes: usize = ev_files.iter().map(|f| f.len()).sum();
    let ck_bytes: usize = ck_files.iter().map(|f| f.len()).sum();
    let lk_bytes: usize = lk_files.iter().map(|f| f.len()).sum();
    if ev_bytes < 1000 {
        return Err("no events".into());
    }
    lg!(t0, "mapped {} event, {} click, {} link files ({} MB)", ev_files.len(), ck_files.len(), lk_files.len(), (ev_bytes + ck_bytes + lk_bytes) >> 20);

    // ---- Early decision: recover the three keys from a sample. ----
    let mut ev_codes = Vec::new();
    let mut u_codes = Vec::new();
    for l in sample_lines(&ev_files, 6000) {
        let mut f = [(0usize, 0usize); 7];
        if csv::split::<7>(l, &mut f) {
            if let Some(c) = idc::code_of(&l[f[0].0..f[0].1], b'e') {
                ev_codes.push(c);
            }
            if let Some(c) = idc::code_of(&l[f[1].0..f[1].1], b'u') {
                u_codes.push(c);
            }
        }
    }
    let mut ck_codes = Vec::new();
    for l in sample_lines(&ck_files, 6000) {
        let mut f = [(0usize, 0usize); 4];
        if csv::split::<4>(l, &mut f) {
            if let Some(c) = idc::code_of(&l[f[0].0..f[0].1], b'k') {
                ck_codes.push(c);
            }
            if let Some(c) = idc::code_of(&l[f[1].0..f[1].1], b'u') {
                u_codes.push(c);
            }
        }
    }
    ev_codes.sort_unstable();
    ev_codes.dedup();
    ck_codes.sort_unstable();
    ck_codes.dedup();
    let ev_bits = ceil_log2(ev_bytes as u64 / 24 + 1).clamp(10, 29);
    let ev_key = idc::find_dense_key(&ev_codes, ev_bits, threads).ok_or("no event key found in the sample")?;
    let u_key = idc::find_user_key(&u_codes, UBITS).ok_or("no user key found in the sample")?;
    let ck_bits = ceil_log2(ck_bytes as u64 / 12 + 1).clamp(8, 28);
    let (ck_key, ck_cap) = match idc::find_dense_key(&ck_codes, ck_bits, threads) {
        Some(k) => (k, 1usize << ck_bits),
        None => {
            if ck_bytes > 4 << 20 {
                return Err("no click key found in the sample".into());
            }
            (0, 0)
        }
    };
    // The keys must explain (almost) the whole sample; exactness never depends on it.
    {
        let ok_e = ev_codes.iter().filter(|&&c| (idc::number(c, ev_key) as usize) < (1usize << ev_bits)).count();
        let ok_u = u_codes.iter().filter(|&&c| (idc::number(c, u_key) >> 9) < UCAP).count();
        if ok_e * 100 < ev_codes.len() * 99 || ok_u * 100 < u_codes.len() * 95 {
            return Err("keys do not explain the sample".into());
        }
    }
    lg!(t0, "keys found (ev bits {ev_bits}, ck cap {ck_cap})");

    // ---- Arrays. ----
    let ev_cap = 1usize << ev_bits;
    let ucap = 1usize << UBITS;


    let ev = mem::zeroed::<EvRec>(ev_cap, true).ok_or("no memory for events")?;
    let ck = mem::zeroed::<CkRec>(ck_cap.max(1), true).ok_or("no memory for clicks")?;
    let words = mem::zeroed(ucap, false).ok_or("no memory")?;
    let slow = mem::zeroed(ucap, false).ok_or("no memory")?;
    let evcnt = mem::zeroed(ucap, false).ok_or("no memory")?;
    let ckcnt = mem::zeroed(ucap, false).ok_or("no memory")?;
    let crun = mem::zeroed(ucap, false).ok_or("no memory")?;
    let cats: &'static [&'static [u8]] = Box::leak(prods.cats.clone().into_boxed_slice());
    let ev_lines_est = ev_bytes / 40;
    let fx = Fx {
        win,
        prods,
        camps,
        ctab,
        ing_base: 0,
        ev,
        ev_cap,
        ev_key,
        ck,
        ck_cap,
        ck_key,
        u_key,
        words,
        slow,
        evcnt,
        ckcnt,
        crun,
        ex_ev: AtomicUsize::new(0),
        ex_ck: AtomicUsize::new(0),
        ex_lk: AtomicUsize::new(0),
        ex_ev_limit: 20_000 + ev_lines_est / 50,
        ex_ck_limit: 20_000 + ck_bytes / 40 / 50,
        ex_lk_limit: 50_000 + lk_bytes / 30 / 10,
    };
    let fx = Fx { ing_base: fx.win.ws.div_euclid(1000) - (1 << 30), ..fx };
    let fx = &fx;

    // ---- Stage P: parse everything into the dense arrays. ----
    let chunk = ((ev_bytes + ck_bytes + lk_bytes) / (threads * 16)).clamp(256 << 10, 8 << 20);
    struct Job {
        kind: u8,
        t: input::Task,
    }
    let mut jobs: Vec<Job> = make_tasks(&lk_files, chunk).into_iter().map(|t| Job { kind: 2, t }).collect();
    jobs.extend(make_tasks(&ck_files, chunk).into_iter().map(|t| Job { kind: 1, t }));
    jobs.extend(make_tasks(&ev_files, chunk).into_iter().map(|t| Job { kind: 0, t }));
    let next = AtomicUsize::new(0);
    let mut tls: Vec<parse::Tl> = par(threads, |_| {
        let mut tl = parse::Tl::default();
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= jobs.len() {
                break;
            }
            let j = &jobs[i];
            match j.kind {
                0 => {
                    let d = ev_files[j.t.file];
                    let (s, e) = j.t.range(d);
                    parse::parse_events(fx, &mut tl, d, s, e);
                }
                1 => {
                    let d = ck_files[j.t.file];
                    let (s, e) = j.t.range(d);
                    parse::parse_clicks(fx, &mut tl, d, s, e);
                }
                _ => {
                    let d = lk_files[j.t.file];
                    let (s, e) = j.t.range(d);
                    parse::parse_links(fx, &mut tl, d, s, e);
                }
            }
        }
        tl
    });
    let nmax_ev = tls.iter().map(|t| t.nmax_ev).max().unwrap_or(0);
    let nmax_ck = tls.iter().map(|t| t.nmax_ck).max().unwrap_or(0);
    lg!(t0, "parsed: {} event slots, {} click slots, extras {}/{}/{}", nmax_ev, nmax_ck, fx.ex_ev.load(Relaxed), fx.ex_ck.load(Relaxed), fx.ex_lk.load(Relaxed));

    let mut slow_users: Vec<u32> = Vec::new();
    for t in tls.iter_mut() {
        slow_users.append(&mut t.slow);
    }

    // Winners with a user that is not a plain generator user become extras; their slots go.
    let mut ex_evs: Vec<parse::ExEv> = Vec::new();
    for t in tls.iter_mut() {
        for o in std::mem::take(&mut t.odd) {
            let r = &fx.ev[o.n as usize];
            if r.v.load(Relaxed) == ODD && r.ing.load(Relaxed) == o.ingw && r.ts.load(Relaxed) != INVALID {
                let ts = fx.win.ws + r.ts.load(Relaxed) as i64;
                let pc = r.pc.load(Relaxed);
                ex_evs.push(parse::ExEv { eid: b"", user: o.user, ing: 0, ts, price: pc >> 8, cat: pc & 255, ok: true, pre: true });
                r.ts.store(INVALID, Relaxed);
            }
        }
    }
    // Event extras: dedup on the raw event_id (smallest ingest_ts wins, then validity).
    {
        let mut best: HashMap<&'static [u8], usize> = HashMap::new();
        let mut all: Vec<parse::ExEv> = Vec::new();
        for t in tls.iter_mut() {
            all.append(&mut t.ex_ev);
        }
        for (i, e) in all.iter().enumerate() {
            match best.get(e.eid) {
                Some(&j) if all[j].ing <= e.ing => {}
                _ => {
                    best.insert(e.eid, i);
                }
            }
        }
        let keep: std::collections::HashSet<usize> = best.values().copied().collect();
        for (i, e) in all.into_iter().enumerate() {
            if keep.contains(&i) && e.ok {
                ex_evs.push(e);
            }
        }
    }
    let mut ex_cks: Vec<parse::ExCk> = Vec::new();
    for t in tls.iter_mut() {
        ex_cks.append(&mut t.ex_ck);
    }
    // Users of extras that are plain generator users join the slow path with their whole run.
    for u in ex_evs.iter().map(|e| e.user).chain(ex_cks.iter().map(|c| c.user)) {
        if let Some(c) = idc::code_of(u, b'u') {
            let v = idc::number(c, fx.u_key);
            if (v >> 9) < UCAP && fx.slow[(v >> 9) as usize].swap(1, Relaxed) == 0 {
                slow_users.push((v >> 9) as u32);
            }
        }
    }
    lg!(t0, "extras resolved: {} events, {} clicks", ex_evs.len(), ex_cks.len());

    // ---- Stage R: runs of consecutive slots per user number. ----
    let nbe = nmax_ev.div_ceil(BLOCK);
    let nbc = nmax_ck.div_ceil(BLOCK);
    let next = AtomicUsize::new(0);
    let scanned: Vec<(Vec<(usize, Vec<Run>)>, Vec<u32>)> = par(threads, |_| {
        let mut got = Vec::new();
        let mut slow = Vec::new();
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= nbe + nbc {
                break;
            }
            if i < nbe {
                got.push((i, scan::event_runs(fx, i, nmax_ev, &mut slow)));
            } else {
                got.push((i, scan::click_runs(fx, i - nbe, nmax_ck, &mut slow)));
            }
        }
        (got, slow)
    });
    let mut blocks: Vec<Vec<Run>> = (0..nbe + nbc).map(|_| Vec::new()).collect();
    for (got, mut slow) in scanned {
        slow_users.append(&mut slow);
        for (i, r) in got {
            blocks[i] = r;
        }
    }
    slow_users.sort_unstable();
    slow_users.dedup();
    let nruns: usize = blocks[..nbe].iter().map(|b| b.len()).sum();
    lg!(t0, "runs: {} event runs, {} slow users", nruns, slow_users.len());
    if slow_users.len() > 20_000 + nruns / 8 {
        return Err(format!("too many users on the slow path ({} of {} runs)", slow_users.len(), nruns));
    }

    // ---- The people of the slow path. ----
    let extra_ids: Vec<&'static [u8]> = ex_evs.iter().map(|e| e.user).chain(ex_cks.iter().map(|c| c.user)).collect();
    let comps = slow::build(fx, &tls, &slow_users, &extra_ids);
    lg!(t0, "slow people built");

    // ---- Stages S1/S2: process users; slow items are bucketed, then processed per bucket. ----
    let day0 = floor_div(fx.win.ws, DAY_MS);
    let day_n = floor_div(fx.win.we - 1, DAY_MS);
    let ndays = if day_n >= day0 { (day_n - day0 + 1) as usize } else { 1 };
    let ncamp = fx.camps.rows.len();
    let organic = fx.camps.organic();
    let user_dir = out_dir.join("user_daily");
    let camp_dir = out_dir.join("campaign_daily");
    for d in [&user_dir, &camp_dir] {
        std::fs::create_dir_all(d).map_err(io)?;
    }
    // Everything the general pipeline would also remove: stale tables from an earlier run.
    for d in [&user_dir, &camp_dir] {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".parquet") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let ntasks = nbe + nbc + 2;
    let next = AtomicUsize::new(0);
    let barrier = Barrier::new(threads);
    let parts: Vec<Mutex<slow::Buckets>> = (0..threads).map(|_| Mutex::new(slow::Buckets::new())).collect();
    let order: OnceLock<Vec<usize>> = OnceLock::new();
    let next_b = AtomicUsize::new(0);
    let slow_events = AtomicUsize::new(0);
    let results: Vec<(CampAcc, u64)> = par(threads, |w| {
        let mut out = Out { users: UserCols::default(), acc: CampAcc::new(ncamp, day0, ndays), worker: w, seq: 0, dir: user_dir.clone(), rows: 0, organic, cats };
        let mut scr = Scratch::default();
        let mut fs = fastuser::FScratch::default();
        let mut bk = slow::Buckets::new();
        let ws = fx.win.ws;
        let cws = fx.win.cws;
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= ntasks {
                break;
            }
            if i < nbe {
                for run in &blocks[i] {
                    if fx.slow[run.u as usize].load(Relaxed) == 0 {
                        fastuser::fast_user(fx, run, &mut fs, &mut scr, &mut out);
                        if out.users.day.len() >= person::FLUSH_ROWS {
                            out.flush();
                        }
                    } else {
                        for s in run.s..run.e {
                            let r = &fx.ev[s as usize];
                            if r.ing.load(Relaxed) == 0 || r.ts.load(Relaxed) == INVALID {
                                continue;
                            }
                            let pc = r.pc.load(Relaxed);
                            let g = comps.group(r.v.load(Relaxed) as u64);
                            bk.push_ev(slow::SlowEv { g, ts: ws + r.ts.load(Relaxed) as i64, price: pc >> 8, cat: pc & 255 });
                        }
                    }
                }
            } else if i < nbe + nbc {
                for run in &blocks[i] {
                    if fx.slow[run.u as usize].load(Relaxed) == 0 {
                        continue;
                    }
                    for s in run.s..run.e {
                        let r = &fx.ck[s as usize];
                        let camp = r.camp.load(Relaxed);
                        if camp == 0 {
                            continue;
                        }
                        let g = comps.group(r.v.load(Relaxed) as u64);
                        bk.push_ck(slow::SlowCk { g, ts: cws + r.ts.load(Relaxed) as i64, camp: camp - 1, len: 0, data: idc::scramble(s as u64, fx.ck_key) });
                    }
                }
            } else if i == nbe + nbc {
                for e in &ex_evs {
                    let g = comps.group(comps.key(fx, e.user));
                    bk.push_ev(slow::SlowEv { g, ts: e.ts, price: e.price, cat: e.cat });
                }
            } else {
                for c in &ex_cks {
                    let g = comps.group(comps.key(fx, c.user));
                    bk.push_ck(slow::SlowCk { g, ts: c.ts, camp: c.camp, len: c.cid.len() as u32, data: c.cid.as_ptr() as u64 });
                }
            }
        }
        slow_events.fetch_add(bk.ev.iter().map(|v| v.len()).sum(), Relaxed);
        *parts[w].lock().unwrap() = bk;
        barrier.wait();
        if w == 0 {
            let mut sizes: Vec<(usize, usize)> = (0..slow::NB)
                .map(|b| (b, parts.iter().map(|p| { let g = p.lock().unwrap(); g.ev[b].len() + g.ck[b].len() }).sum::<usize>()))
                .collect();
            sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
            let _ = order.set(sizes.into_iter().map(|s| s.0).collect());
        }
        barrier.wait();
        let order = order.get().unwrap();
        loop {
            let i = next_b.fetch_add(1, Relaxed);
            if i >= slow::NB {
                break;
            }
            let b = order[i];
            let mut evs = Vec::new();
            let mut cks = Vec::new();
            for p in &parts {
                let mut g = p.lock().unwrap();
                evs.append(&mut g.ev[b]);
                cks.append(&mut g.ck[b]);
            }
            if !evs.is_empty() {
                slow::process_bucket(&comps, evs, cks, &mut scr, &mut out);
                if out.users.day.len() >= person::FLUSH_ROWS {
                    out.flush();
                }
            }
        }
        out.flush();
        (out.acc, out.rows)
    });
    let total_rows: u64 = results.iter().map(|r| r.1).sum();
    lg!(t0, "users done: {} rows, {} slow events", total_rows, slow_events.load(Relaxed));
    if slow_events.load(Relaxed) > 1_000_000 + nruns * 4 {
        // Not exact-affecting, only a performance statement; kept as a log line.
        lg!(t0, "note: slow path is large");
    }

    let mut total = CampAcc::new(ncamp, day0, ndays);
    for (a, _) in results {
        total.merge(a);
    }
    if total_rows == 0 {
        let path = user_dir.join("ssf-empty.parquet");
        handoff::register_output(&path);
        let u = UserCols::default();
        parquet::write_file(
            &path,
            &["user_id", "day", "sessions", "events", "revenue_cents", "top_category"],
            &[Col::Str(&u.user), Col::Date(&u.day), Col::I32(&u.sessions), Col::I32(&u.events), Col::I64(&u.revenue), Col::OptStr(&u.top)],
        )
        .map_err(io)?;
    }
    let mut rows = total.rows();
    rows.sort_unstable_by_key(|r| (r.0, r.1));
    let mut cid = StrCol::default();
    let mut chan = StrCol::default();
    let mut day = Vec::new();
    let (mut c_s, mut c_p, mut c_r, mut c_a) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (camp, d, c) in &rows {
        let (id, ch) = fx.camps.rows[*camp as usize];
        cid.push(id);
        chan.push(ch);
        day.push(*d);
        c_s.push(c[acc::SESSIONS] as i64);
        c_p.push(c[acc::PURCHASING] as i64);
        c_r.push(c[acc::REVENUE] as i64);
        c_a.push(c[acc::ASSISTED] as i64);
    }
    let cpath = camp_dir.join("ssf-000.parquet");
    handoff::register_output(&cpath);
    parquet::write_file(
        &cpath,
        &["campaign_id", "channel", "day", "sessions", "purchasing_sessions", "revenue_cents", "assisted_sessions"],
        &[Col::Str(&cid), Col::Str(&chan), Col::Date(&day), Col::I64(&c_s), Col::I64(&c_p), Col::I64(&c_r), Col::I64(&c_a)],
    )
    .map_err(io)?;
    lg!(t0, "done");
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => unsafe { _exit(0) },
        Err(e) => give_up(&e),
    }
}
