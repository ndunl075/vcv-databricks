//! Linked IDs: union-find over exact IDs and the lookup table used for events and clicks.

use crate::util::hash128;

/// One side of a link line: 128-bit hash identity plus where its bytes live.
#[derive(Clone, Copy)]
pub struct IdRef {
    pub lo: u64,
    pub hi: u64,
    pub ptr: u64,
    pub len: u32,
}

#[derive(Clone, Copy)]
pub struct LinkRec {
    pub a: IdRef,
    pub b: IdRef,
}

impl IdRef {
    #[inline]
    pub fn new(s: &'static [u8]) -> IdRef {
        let (lo, hi) = hash128(s);
        IdRef { lo, hi, ptr: s.as_ptr() as u64, len: s.len() as u32 }
    }
    #[inline]
    pub fn bytes(&self) -> &'static [u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as usize as *const u8, self.len as usize) }
    }
}

/// The person's name data: the smallest ID of the component.
#[derive(Clone, Copy, Default)]
pub struct Comp {
    pub lo: u64,
    pub hi: u64,
    pub ptr: u64,
    pub len: u32,
    pub _pad: u32,
}

impl Comp {
    pub fn bytes(&self) -> &'static [u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as usize as *const u8, self.len as usize) }
    }
}

pub struct LinkTable {
    /// (key lo, key hi, comp index + 1); 0 = empty.
    slots: Vec<[u64; 3]>,
    mask: usize,
    pub comps: Vec<Comp>,
}

impl LinkTable {
    pub fn empty() -> LinkTable {
        LinkTable { slots: vec![[0; 3]; 1], mask: 0, comps: Vec::new() }
    }

    /// Hints the CPU to load the table slot an upcoming `get(lo, ..)` will probe first.
    #[inline(always)]
    pub fn prefetch(&self, lo: u64) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
            let i = (lo as usize) & self.mask;
            _mm_prefetch(self.slots.as_ptr().add(i) as *const i8, _MM_HINT_T0);
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = lo;
    }

    /// Component index of an ID that appears in some link line.
    #[inline]
    pub fn get(&self, lo: u64, hi: u64) -> Option<u32> {
        let mut i = (lo as usize) & self.mask;
        loop {
            let s = &self.slots[i];
            if s[2] == 0 {
                return None;
            }
            if s[0] == lo && s[1] == hi {
                return Some((s[2] - 1) as u32);
            }
            i = (i + 1) & self.mask;
        }
    }

    pub fn build(parts: &[Vec<LinkRec>]) -> LinkTable {
        let nrec: usize = parts.iter().map(|p| p.len()).sum();
        if nrec == 0 {
            return LinkTable::empty();
        }
        let mut cap = (nrec * 2).next_power_of_two().max(16);
        let mut slots: Vec<[u64; 3]> = vec![[0; 3]; cap];
        let mut ids: Vec<IdRef> = Vec::with_capacity(nrec);
        let mut parent: Vec<u32> = Vec::with_capacity(nrec);
        let mut size: Vec<u32> = Vec::with_capacity(nrec);
        let mut best: Vec<u32> = Vec::with_capacity(nrec);

        fn find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                let p = parent[x as usize];
                parent[x as usize] = parent[p as usize];
                x = parent[x as usize];
            }
            x
        }

        for part in parts {
            for r in part {
                let mut idx2 = [0u32; 2];
                for (k, id) in [r.a, r.b].iter().enumerate() {
                    let mask = cap - 1;
                    let mut i = (id.lo as usize) & mask;
                    let idx = loop {
                        let s = slots[i];
                        if s[2] == 0 {
                            let idx = ids.len() as u32;
                            slots[i] = [id.lo, id.hi, idx as u64 + 1];
                            ids.push(*id);
                            parent.push(idx);
                            size.push(1);
                            best.push(idx);
                            break idx;
                        }
                        if s[0] == id.lo && s[1] == id.hi {
                            break (s[2] - 1) as u32;
                        }
                        i = (i + 1) & mask;
                    };
                    idx2[k] = idx;
                    if ids.len() * 2 > cap {
                        cap *= 2;
                        let mut ns: Vec<[u64; 3]> = vec![[0; 3]; cap];
                        let nm = cap - 1;
                        for s in slots.iter().filter(|s| s[2] != 0) {
                            let mut j = (s[0] as usize) & nm;
                            while ns[j][2] != 0 {
                                j = (j + 1) & nm;
                            }
                            ns[j] = *s;
                        }
                        slots = ns;
                    }
                }
                let ra = find(&mut parent, idx2[0]);
                let rb = find(&mut parent, idx2[1]);
                if ra != rb {
                    let (big, small) = if size[ra as usize] >= size[rb as usize] { (ra, rb) } else { (rb, ra) };
                    parent[small as usize] = big;
                    size[big as usize] += size[small as usize];
                    let bm = best[big as usize];
                    let sm = best[small as usize];
                    if ids[sm as usize].bytes() < ids[bm as usize].bytes() {
                        best[big as usize] = sm;
                    }
                }
            }
        }
        let n = ids.len();
        let mut comps = vec![Comp::default(); n];
        for i in 0..n {
            if parent[i] == i as u32 {
                let m = ids[best[i] as usize];
                comps[i] = Comp { lo: m.lo, hi: m.hi, ptr: m.ptr, len: m.len, _pad: 0 };
            }
        }
        for s in slots.iter_mut().filter(|s| s[2] != 0) {
            let root = find(&mut parent, (s[2] - 1) as u32);
            s[2] = root as u64 + 1;
        }
        LinkTable { slots, mask: cap - 1, comps }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &'static str) -> IdRef {
        IdRef::new(s.as_bytes())
    }

    #[test]
    fn chains_and_min_name() {
        let recs = vec![
            LinkRec { a: id("u3"), b: id("u9") },
            LinkRec { a: id("u9"), b: id("u2") },
            LinkRec { a: id("U7"), b: id("u9") },
            LinkRec { a: id("u10"), b: id("u10") },
            LinkRec { a: id("x"), b: id("y") },
        ];
        let t = LinkTable::build(&[recs]);
        let g = |s: &str| {
            let h = hash128(s.as_bytes());
            t.get(h.0, h.1).map(|c| t.comps[c as usize].bytes().to_vec())
        };
        assert_eq!(g("u3").unwrap(), b"U7");
        assert_eq!(g("u9").unwrap(), b"U7");
        assert_eq!(g("u2").unwrap(), b"U7");
        assert_eq!(g("u10").unwrap(), b"u10");
        assert_eq!(g("y").unwrap(), b"x");
        assert!(g("u1").is_none());
    }
}
