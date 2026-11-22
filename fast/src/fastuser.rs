//! The fast path for one user number: one run of events, one run of clicks, a link word.

use crate::fx::*;
use crate::handoff::give_up;
use crate::idc::{lex_key, scramble, write_id};
use crate::parse::word_find;
use crate::person::{person, Out, Scratch, C, E};
use crate::scan::Run;
use std::sync::atomic::Ordering::Relaxed;

#[derive(Clone, Copy)]
struct FE {
    ts: i64,
    pc: u32,
    alias: u32,
}

#[derive(Clone, Copy)]
struct FC {
    ts: i64,
    n: u32,
    camp: u16,
    alias: u16,
}

#[derive(Default)]
pub struct FScratch {
    items: Vec<FE>,
    cks: Vec<FC>,
    evs: Vec<E>,
    cs: Vec<C>,
}

/// Name of a person: the byte-wise smallest ID among the aliases joined to `root`.
fn name_of(fx: &Fx, u: u64, word: u64, root: u32, buf: &mut [u8; 10]) -> usize {
    let mut best = scramble(u * 512 + root as u64, fx.u_key);
    if word != 0 {
        let mut bk = lex_key(best);
        for b in 0..NALIAS {
            if b != root && word_find(word, b) == root {
                let c = scramble(u * 512 + b as u64, fx.u_key);
                let k = lex_key(c);
                if k < bk {
                    bk = k;
                    best = c;
                }
            }
        }
    }
    write_id(buf, b'u', best)
}

pub fn fast_user(fx: &Fx, run: &Run, fs: &mut FScratch, scr: &mut Scratch, out: &mut Out) {
    let u = run.u as u64;
    let word = fx.words[run.u as usize].load(Relaxed);
    let (ws, cws) = (fx.win.ws, fx.win.cws);
    fs.items.clear();
    let mut sorted = true;
    let mut last = 0i64;
    for i in run.s..run.e {
        let r = &fx.ev[i as usize];
        if r.ing.load(Relaxed) == 0 {
            continue;
        }
        let ts = r.ts.load(Relaxed);
        if ts == INVALID {
            continue;
        }
        let alias = r.v.load(Relaxed) & 511;
        if alias >= NALIAS {
            give_up("alias beyond the link word on the fast path");
        }
        let t = ws + ts as i64;
        if t < last {
            sorted = false;
        }
        last = t;
        fs.items.push(FE { ts: t, pc: r.pc.load(Relaxed), alias });
    }
    if fs.items.is_empty() {
        return;
    }
    if !sorted {
        fs.items.sort_by_key(|e| e.ts);
    }
    fs.cks.clear();
    let cr = fx.crun[run.u as usize].load(Relaxed);
    if cr != 0 {
        let (cs, ce) = ((cr >> 32) as usize, (cr & 0xFFFF_FFFF) as usize);
        for i in cs..ce {
            let r = &fx.ck[i];
            let camp = r.camp.load(Relaxed);
            if camp == 0 {
                continue;
            }
            let alias = r.v.load(Relaxed) & 511;
            if alias >= NALIAS {
                give_up("click alias beyond the link word on the fast path");
            }
            fs.cks.push(FC { ts: cws + r.ts.load(Relaxed) as i64, n: i as u32, camp: (camp - 1) as u16, alias: alias as u16 });
        }
        fs.cks.sort_unstable_by_key(|c| c.ts);
        // Equal timestamps: the smallest click_id (byte-wise) first.
        let mut a = 0;
        while a < fs.cks.len() {
            let mut b = a + 1;
            while b < fs.cks.len() && fs.cks[b].ts == fs.cks[a].ts {
                b += 1;
            }
            if b - a > 1 {
                fs.cks[a..b].sort_unstable_by_key(|c| lex_key(scramble(c.n as u64, fx.ck_key)));
            }
            a = b;
        }
    }
    let root = |alias: u32| if word == 0 { alias } else { word_find(word, alias) };
    let r0 = root(fs.items[0].alias);
    let single = fs.items.iter().all(|e| root(e.alias) == r0);
    let mut buf = [0u8; 10];
    if single {
        fs.evs.clear();
        fs.evs.extend(fs.items.iter().map(|e| E { ts: e.ts, price: e.pc >> 8, cat: e.pc & 255 }));
        fs.cs.clear();
        fs.cs.extend(fs.cks.iter().filter(|c| root(c.alias as u32) == r0).map(|c| C { ts: c.ts, camp: c.camp as u32 }));
        let n = name_of(fx, u, word, r0, &mut buf);
        person(&buf[..n], &fs.evs, &fs.cs, scr, out);
    } else {
        // Several people in one user number (links missing or split them): by root, stable.
        fs.items.sort_by_key(|e| root(e.alias));
        let mut a = 0;
        while a < fs.items.len() {
            let r = root(fs.items[a].alias);
            let mut b = a + 1;
            while b < fs.items.len() && root(fs.items[b].alias) == r {
                b += 1;
            }
            fs.evs.clear();
            fs.evs.extend(fs.items[a..b].iter().map(|e| E { ts: e.ts, price: e.pc >> 8, cat: e.pc & 255 }));
            fs.cs.clear();
            fs.cs.extend(fs.cks.iter().filter(|c| root(c.alias as u32) == r).map(|c| C { ts: c.ts, camp: c.camp as u32 }));
            let n = name_of(fx, u, word, r, &mut buf);
            person(&buf[..n], &fs.evs, &fs.cs, scr, out);
            a = b;
        }
    }
}
