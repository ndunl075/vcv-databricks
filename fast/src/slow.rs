//! The slow side path: exact, hash-based, for everything the fast path does not take:
//! users with several runs, users joined by cross-user or odd links, odd IDs, chains.
//!
//! People are components of a union-find over keys: a canonical user ID is its number
//! (`user * 512 + alias`, below 2^40); any other ID is interned and gets a key above 2^62.

use crate::fx::*;
use crate::idc::{scramble, write_id};
use crate::parse::{user_v_any, word_find, Tl};
use crate::person::{person, Out, Scratch, C, E};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Cheap, well-mixed hasher for u64 keys.
#[derive(Default)]
pub struct Mix(u64);
impl Hasher for Mix {
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    #[inline]
    fn write_u64(&mut self, x: u64) {
        let mut z = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z ^= z >> 29;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        self.0 = z ^ (z >> 32);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}
type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<Mix>>;
use std::sync::atomic::Ordering::Relaxed;

const ODD_BASE: u64 = 1 << 62;
pub const NB: usize = 256;

#[derive(Clone, Copy)]
pub struct SlowEv {
    pub g: u64,
    pub ts: i64,
    pub price: u32,
    pub cat: u32,
}

#[derive(Clone, Copy)]
pub struct SlowCk {
    pub g: u64,
    pub ts: i64,
    pub camp: u32,
    /// 0: a canonical click ID with code `data`; else `data` is the address of `len` bytes.
    pub len: u32,
    pub data: u64,
}

impl SlowCk {
    fn id<'a>(&self, buf: &'a mut [u8; 10]) -> &'a [u8] {
        if self.len == 0 {
            let n = write_id(buf, b'k', self.data);
            &buf[..n]
        } else {
            unsafe { std::slice::from_raw_parts(self.data as usize as *const u8, self.len as usize) }
        }
    }
}

pub struct Comps {
    map: FastMap<u64, u32>,
    rep: Vec<u64>,
    names: Vec<Vec<u8>>,
    odd: Vec<&'static [u8]>,
    oddmap: HashMap<&'static [u8], u64>,
    u_key: u64,
}

impl Comps {
    /// Key of a user ID; interns an odd ID. Single-threaded use only.
    pub fn intern(&mut self, fx: &Fx, b: &'static [u8]) -> u64 {
        if let Some(v) = user_v_any(fx, b) {
            return v;
        }
        if let Some(&k) = self.oddmap.get(b) {
            return k;
        }
        let k = ODD_BASE + self.odd.len() as u64;
        self.odd.push(b);
        self.oddmap.insert(b, k);
        k
    }

    /// Key of an ID already seen by `intern`.
    pub fn key(&self, fx: &Fx, b: &[u8]) -> u64 {
        if let Some(v) = user_v_any(fx, b) {
            return v;
        }
        match self.oddmap.get(b) {
            Some(&k) => k,
            None => crate::handoff::give_up("slow path: ID not interned"),
        }
    }

    fn key_bytes(&self, k: u64, out: &mut Vec<u8>) {
        out.clear();
        if k >= ODD_BASE {
            out.extend_from_slice(self.odd[(k - ODD_BASE) as usize]);
        } else {
            let mut buf = [0u8; 10];
            let n = write_id(&mut buf, b'u', scramble(k, self.u_key));
            out.extend_from_slice(&buf[..n]);
        }
    }

    /// The person's representative key (the key itself when it has no links).
    #[inline]
    pub fn group(&self, k: u64) -> u64 {
        match self.map.get(&k) {
            Some(&c) => self.rep[c as usize],
            None => k,
        }
    }

    pub fn name_of_group(&self, g: u64, out: &mut Vec<u8>) {
        match self.map.get(&g) {
            Some(&c) => {
                out.clear();
                out.extend_from_slice(&self.names[c as usize]);
            }
            None => self.key_bytes(g, out),
        }
    }
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[x as usize];
        parent[x as usize] = parent[p as usize];
        x = parent[x as usize];
    }
    x
}

/// Builds the people of the slow path from the complex links, the link words of the slow
/// users, and interns every odd ID in the extras.
pub fn build(fx: &Fx, tls: &[Tl], slow_users: &[u32], extra_users: &[&'static [u8]]) -> Comps {
    let mut c = Comps { map: FastMap::default(), rep: Vec::new(), names: Vec::new(), odd: Vec::new(), oddmap: HashMap::new(), u_key: fx.u_key };
    for u in extra_users {
        c.intern(fx, u);
    }
    let mut nodes: FastMap<u64, u32> = FastMap::default();
    let mut keys: Vec<u64> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    let mut node = |k: u64, keys: &mut Vec<u64>, parent: &mut Vec<u32>| -> u32 {
        *nodes.entry(k).or_insert_with(|| {
            keys.push(k);
            parent.push(parent.len() as u32);
            (parent.len() - 1) as u32
        })
    };
    let mut edge = |a: u64, b: u64, keys: &mut Vec<u64>, parent: &mut Vec<u32>| {
        let (x, y) = (node(a, keys, parent), node(b, keys, parent));
        let (rx, ry) = (find(parent, x), find(parent, y));
        if rx != ry {
            parent[ry as usize] = rx;
        }
    };
    for tl in tls {
        for l in &tl.ex_lk {
            let (a, b) = (c.intern(fx, l.a), c.intern(fx, l.b));
            edge(a, b, &mut keys, &mut parent);
        }
    }
    for &u in slow_users {
        let w = fx.words[u as usize].load(Relaxed);
        if w != 0 {
            for a in 0..NALIAS {
                let r = word_find(w, a);
                if r != a {
                    edge(u as u64 * 512 + a as u64, u as u64 * 512 + r as u64, &mut keys, &mut parent);
                }
            }
        }
    }
    // Components, named by their byte-wise smallest member.
    let mut comp_of_root: FastMap<u32, u32> = FastMap::default();
    let mut tmp = Vec::new();
    for i in 0..keys.len() as u32 {
        let r = find(&mut parent, i);
        let ci = *comp_of_root.entry(r).or_insert_with(|| {
            c.rep.push(keys[r as usize]);
            c.names.push(Vec::new());
            (c.rep.len() - 1) as u32
        });
        c.map.insert(keys[i as usize], ci);
        c.key_bytes(keys[i as usize], &mut tmp);
        let name = &mut c.names[ci as usize];
        if name.is_empty() || tmp < *name {
            *name = tmp.clone();
        }
    }
    c
}

pub struct Buckets {
    pub ev: Vec<Vec<SlowEv>>,
    pub ck: Vec<Vec<SlowCk>>,
}

impl Buckets {
    pub fn new() -> Buckets {
        Buckets { ev: (0..NB).map(|_| Vec::new()).collect(), ck: (0..NB).map(|_| Vec::new()).collect() }
    }
    #[inline]
    fn slot(g: u64) -> usize {
        (g.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize
    }
    #[inline]
    pub fn push_ev(&mut self, e: SlowEv) {
        self.ev[Self::slot(e.g)].push(e);
    }
    #[inline]
    pub fn push_ck(&mut self, c: SlowCk) {
        self.ck[Self::slot(c.g)].push(c);
    }
}

/// One bucket: sort, walk the people, process them.
pub fn process_bucket(comps: &Comps, mut evs: Vec<SlowEv>, mut cks: Vec<SlowCk>, scr: &mut Scratch, out: &mut Out) {
    evs.sort_unstable_by_key(|e| (e.g, e.ts));
    cks.sort_unstable_by(|a, b| {
        (a.g, a.ts).cmp(&(b.g, b.ts)).then_with(|| {
            let (mut x, mut y) = ([0u8; 10], [0u8; 10]);
            a.id(&mut x).cmp(b.id(&mut y))
        })
    });
    let mut name = Vec::new();
    let mut es: Vec<E> = Vec::new();
    let mut cs: Vec<C> = Vec::new();
    let mut ci = 0;
    let mut i = 0;
    while i < evs.len() {
        let g = evs[i].g;
        let mut j = i + 1;
        while j < evs.len() && evs[j].g == g {
            j += 1;
        }
        while ci < cks.len() && cks[ci].g < g {
            ci += 1;
        }
        let mut ce = ci;
        while ce < cks.len() && cks[ce].g == g {
            ce += 1;
        }
        es.clear();
        es.extend(evs[i..j].iter().map(|e| E { ts: e.ts, price: e.price, cat: e.cat }));
        cs.clear();
        cs.extend(cks[ci..ce].iter().map(|c| C { ts: c.ts, camp: c.camp }));
        comps.name_of_group(g, &mut name);
        person(&name, &es, &cs, scr, out);
        ci = ce;
        i = j;
    }
}
