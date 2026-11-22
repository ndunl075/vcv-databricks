//! Line parsers for events, clicks and links.

use crate::buckets::Buckets;
use crate::csv::{for_each_line, split};
use crate::links::{IdRef, LinkRec};
use crate::rec::*;
use crate::ts::{parse_price, parse_ts};
use crate::util::{hash128, DAY_MS};

/// Parses event lines of one chunk into per-eshard buckets.
pub fn parse_events(cx: &Ctx, data: &'static [u8], s: usize, e: usize, buckets: &mut [Buckets<Ev>]) {
    for_each_line(data, s, e, |line| {
        let mut f = [(0usize, 0usize); 7];
        if !split::<7>(line, &mut f) {
            return;
        }
        let eid = &line[f[0].0..f[0].1];
        if eid.is_empty() {
            return;
        }
        let eh = hash128(eid).0;
        let user = &line[f[1].0..f[1].1];
        let ts = parse_ts(&line[f[2].0..f[2].1]);
        let ing = parse_ts(&line[f[3].0..f[3].1]);
        let ty = &line[f[4].0..f[4].1];
        let prod = &line[f[5].0..f[5].1];

        let mut ok = !user.is_empty() && !prod.is_empty();
        let ts_v = match ts {
            Some(t) if t >= cx.win.ws && t < cx.win.we => t,
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
            b"purchase" => match parse_price(&line[f[6].0..f[6].1]) {
                Some(p) => {
                    price = p;
                    cat = match cx.prods.map.get(prod) {
                        Some(pi) => cx.prods.cat_of[pi as usize],
                        None => cx.prods.unknown,
                    };
                }
                None => ok = false,
            },
            _ => ok = false,
        }
        if ok && ing_v.checked_sub(ts_v).map_or(true, |d| d > DAY_MS) {
            ok = false;
        }
        let (ulo, uhi, uptr) = if ok {
            let h = hash128(user);
            (h.0, h.1, user.as_ptr() as u64)
        } else {
            (0, 0, 0)
        };
        let ev = Ev {
            eh,
            ep: (eid.as_ptr() as u64 & PTR_MASK) | if ok { OK_BIT } else { 0 },
            ing: ing_v,
            ts: ts_v,
            ulo,
            uhi,
            uptr,
            price,
            cat,
        };
        buckets[cx.eshard(eh)].push(ev);
    });
}

/// Parses click lines (valid clicks only); the person tag comes later.
pub fn parse_clicks(cx: &Ctx, data: &'static [u8], s: usize, e: usize, out: &mut Buckets<SClick>) {
    for_each_line(data, s, e, |line| {
        let mut f = [(0usize, 0usize); 4];
        if !split::<4>(line, &mut f) {
            return;
        }
        let cid = &line[f[0].0..f[0].1];
        let user = &line[f[1].0..f[1].1];
        if cid.is_empty() || user.is_empty() {
            return;
        }
        let ts = match parse_ts(&line[f[3].0..f[3].1]) {
            Some(t) if t >= cx.win.cws && t < cx.win.we => t,
            _ => return,
        };
        let camp = match cx.camps.map.get(&line[f[2].0..f[2].1]) {
            Some(c) => c,
            None => return,
        };
        let (ulo, uhi) = hash128(user);
        // `lo`/`hi` hold the user's ID hash until the person is known (stage 3).
        out.push(SClick { lo: ulo, hi: uhi, ts, cid: cid.as_ptr() as u64, camp, _pad: 0 });
    });
}

/// Parses link lines: both sides non-empty.
pub fn parse_links(data: &'static [u8], s: usize, e: usize, out: &mut Vec<LinkRec>) {
    for_each_line(data, s, e, |line| {
        let mut f = [(0usize, 0usize); 2];
        if !split::<2>(line, &mut f) {
            return;
        }
        let a = &line[f[0].0..f[0].1];
        let b = &line[f[1].0..f[1].1];
        if a.is_empty() || b.is_empty() {
            return;
        }
        out.push(LinkRec { a: IdRef::new(a), b: IdRef::new(b) });
    });
}
