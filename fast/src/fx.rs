//! Shared context and the fixed-size records of the fast path.

use crate::tables::{Campaigns, Products, Window};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize};

/// User numbers (before the alias) must be below `UCAP`; larger ones are carried as "odd".
pub const UBITS: u32 = 23;
pub const UCAP: u64 = (1 << UBITS) - 1;
/// Aliases below this are tracked in the per-user link word; others go to the slow path.
pub const NALIAS: u32 = 16;
pub const ODD: u32 = u32::MAX;
pub const LOCK: u32 = u32::MAX;
pub const INVALID: u32 = u32::MAX;
pub const BAD_ING: u32 = u32::MAX - 1;
/// Slots per block of the run scan.
pub const BLOCK: usize = 1 << 16;

/// Dense event slot, by event number. `ing` is 0 for an empty slot, `LOCK` while written,
/// else the ingest second (relative, plus one) of the kept copy.
#[repr(C, align(16))]
pub struct EvRec {
    pub ing: AtomicU32,
    /// Milliseconds after the window start, or `INVALID` if the kept copy is invalid.
    pub ts: AtomicU32,
    /// User number * 512 + alias, or `ODD`.
    pub v: AtomicU32,
    /// Price (bits 8..32, purchases only) and category rank (bits 0..8).
    pub pc: AtomicU32,
}

/// Dense click slot, by click number. Only valid clicks are stored.
#[repr(C)]
pub struct CkRec {
    /// Milliseconds after `window_start - 7 days`.
    pub ts: AtomicU32,
    pub v: AtomicU32,
    /// Campaign index + 1; 0 for an empty slot.
    pub camp: AtomicU32,
}

pub struct CampTab {
    /// Campaign number -> index + 1 (0: no such campaign).
    pub tab: Vec<u16>,
}

impl CampTab {
    /// Builds the number table; `None` unless every campaign id is `c_` + canonical decimal.
    pub fn build(c: &Campaigns) -> Option<CampTab> {
        let n = c.rows.len() - 1; // the last row is `organic`
        if n >= 60_000 {
            return None;
        }
        let mut tab = vec![0u16; 1];
        for (i, (id, _)) in c.rows[..n].iter().enumerate() {
            let num = Self::number(id)? as usize;
            if num >= 65_000 {
                return None;
            }
            if tab.len() <= num {
                tab.resize(num + 1, 0);
            }
            if tab[num] == 0 {
                tab[num] = (i + 1) as u16;
            }
        }
        Some(CampTab { tab })
    }

    #[inline]
    fn number(b: &[u8]) -> Option<u32> {
        let n = b.len();
        if !(3..=7).contains(&n) || b[0] != b'c' || b[1] != b'_' {
            return None;
        }
        if b[2] == b'0' && n > 3 {
            return None;
        }
        let mut v = 0u32;
        for &c in &b[2..] {
            let d = c.wrapping_sub(b'0');
            if d > 9 {
                return None;
            }
            v = v * 10 + d as u32;
        }
        Some(v)
    }

    #[inline]
    pub fn lookup(&self, b: &[u8]) -> Option<u32> {
        let v = Self::number(b)? as usize;
        match self.tab.get(v) {
            Some(&i) if i != 0 => Some(i as u32 - 1),
            _ => None,
        }
    }
}

pub struct Fx {
    pub win: Window,
    pub prods: Products,
    pub camps: Campaigns,
    pub ctab: CampTab,
    /// `ws/1000 - 2^30`: ingest seconds are stored relative to this.
    pub ing_base: i64,
    pub ev: &'static [EvRec],
    pub ev_cap: usize,
    pub ev_key: u64,
    pub ck: &'static [CkRec],
    pub ck_cap: usize,
    pub ck_key: u64,
    pub u_key: u64,
    /// Per user number: union-find word over the first 16 aliases (see `parse::word_*`).
    pub words: &'static [AtomicU64],
    /// Users that need the slow path.
    pub slow: &'static [AtomicU8],
    pub evcnt: &'static [AtomicU8],
    pub ckcnt: &'static [AtomicU8],
    /// Per user number: click run as `start << 32 | end` (slot numbers), 0 if none.
    pub crun: &'static [AtomicU64],
    pub ex_ev: AtomicUsize,
    pub ex_ck: AtomicUsize,
    pub ex_lk: AtomicUsize,
    pub ex_ev_limit: usize,
    pub ex_ck_limit: usize,
    pub ex_lk_limit: usize,
}
