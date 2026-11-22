//! Campaign-day accumulators.

use std::collections::HashMap;

pub struct CampAcc {
    dense: Vec<[u64; 4]>,
    ndays: usize,
    day0: i64,
    sparse: HashMap<(u32, i32), [u64; 4]>,
    use_dense: bool,
    pub ncamp: usize,
}

pub const SESSIONS: usize = 0;
pub const PURCHASING: usize = 1;
pub const REVENUE: usize = 2;
pub const ASSISTED: usize = 3;

impl CampAcc {
    pub fn new(ncamp: usize, day0: i64, ndays: usize) -> CampAcc {
        let cells = ncamp.saturating_mul(ndays);
        let use_dense = cells <= 1_000_000;
        CampAcc {
            dense: if use_dense { vec![[0; 4]; cells] } else { Vec::new() },
            ndays,
            day0,
            sparse: HashMap::new(),
            use_dense,
            ncamp,
        }
    }
    #[inline]
    fn cell(&mut self, camp: u32, day: i32) -> &mut [u64; 4] {
        if self.use_dense {
            let d = (day as i64 - self.day0) as usize;
            &mut self.dense[camp as usize * self.ndays + d]
        } else {
            self.sparse.entry((camp, day)).or_default()
        }
    }
    #[inline]
    pub fn credit(&mut self, camp: u32, day: i32, purchased: bool, revenue: u64) {
        let c = self.cell(camp, day);
        c[SESSIONS] += 1;
        c[PURCHASING] += purchased as u64;
        c[REVENUE] += revenue;
    }
    #[inline]
    pub fn assist(&mut self, camp: u32, day: i32, n: u64) {
        self.cell(camp, day)[ASSISTED] += n;
    }
    /// Adds `other` into self.
    pub fn merge(&mut self, other: CampAcc) {
        if other.use_dense {
            for (i, c) in other.dense.iter().enumerate() {
                if c.iter().any(|&v| v != 0) {
                    let camp = (i / other.ndays) as u32;
                    let day = (i % other.ndays) as i64 + other.day0;
                    let t = self.cell(camp, day as i32);
                    for k in 0..4 {
                        t[k] += c[k];
                    }
                }
            }
        } else {
            for ((camp, day), c) in other.sparse {
                let t = self.cell(camp, day);
                for k in 0..4 {
                    t[k] += c[k];
                }
            }
        }
    }
    /// All non-empty cells as (camp, day, counters).
    pub fn rows(&self) -> Vec<(u32, i32, [u64; 4])> {
        let mut v = Vec::new();
        if self.use_dense {
            for (i, c) in self.dense.iter().enumerate() {
                if c[SESSIONS] != 0 || c[ASSISTED] != 0 {
                    v.push(((i / self.ndays) as u32, ((i % self.ndays) as i64 + self.day0) as i32, *c));
                }
            }
        } else {
            for (&(camp, day), c) in &self.sparse {
                if c[SESSIONS] != 0 || c[ASSISTED] != 0 {
                    v.push((camp, day, *c));
                }
            }
        }
        v
    }
}
