//! Small side inputs: manifest, products, campaigns, and the exact string map used for them.

use crate::csv::{for_each_line, split};
use crate::input::{first_row, open_view};
use crate::ts::epoch_ms;
use crate::util::{find_byte, hash128, WEEK_MS};
use std::path::Path;

/// Exact byte-string -> u32 map (open addressing, compares bytes).
pub struct StrMap {
    slots: Vec<(u64, &'static [u8], u32)>,
    mask: usize,
}

impl StrMap {
    pub fn with_capacity(n: usize) -> StrMap {
        let cap = (n * 2).next_power_of_two().max(16);
        StrMap { slots: vec![(0, &[][..], u32::MAX); cap], mask: cap - 1 }
    }
    /// Inserts if absent; returns false if the key was already present (first value kept).
    pub fn insert(&mut self, key: &'static [u8], val: u32) -> bool {
        let h = hash128(key).0;
        let mut i = (h as usize) & self.mask;
        loop {
            let s = &self.slots[i];
            if s.2 == u32::MAX {
                self.slots[i] = (h, key, val);
                return true;
            }
            if s.0 == h && s.1 == key {
                return false;
            }
            i = (i + 1) & self.mask;
        }
    }
    #[inline]
    pub fn get(&self, key: &[u8]) -> Option<u32> {
        let h = hash128(key).0;
        let mut i = (h as usize) & self.mask;
        loop {
            let s = &self.slots[i];
            if s.2 == u32::MAX {
                return None;
            }
            if s.0 == h && s.1 == key {
                return Some(s.2);
            }
            i = (i + 1) & self.mask;
        }
    }
}

pub struct Window {
    pub ws: i64,
    pub we: i64,
    /// Click window start: `ws - 7 days`.
    pub cws: i64,
}

fn json_str_field<'a>(text: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let pat = format!("\"{key}\"");
    let pb = pat.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].windows(pb.len()).position(|w| w == pb) {
        let mut p = from + i + pb.len();
        while p < text.len() && text[p].is_ascii_whitespace() {
            p += 1;
        }
        if p < text.len() && text[p] == b':' {
            p += 1;
            while p < text.len() && text[p].is_ascii_whitespace() {
                p += 1;
            }
            if p < text.len() && text[p] == b'"' {
                let s = p + 1;
                let e = find_byte(text, s, b'"')?;
                return Some(&text[s..e]);
            }
        }
        from += i + 1;
    }
    None
}

pub fn read_manifest(dir: &Path) -> Result<Window, String> {
    let text = std::fs::read(dir.join("manifest.json")).map_err(|e| format!("manifest.json: {e}"))?;
    let ws = json_str_field(&text, "window_start").ok_or("manifest: no window_start")?;
    let we = json_str_field(&text, "window_end").ok_or("manifest: no window_end")?;
    let ws = epoch_ms(ws).ok_or("manifest: bad window_start")?;
    let we = epoch_ms(we).ok_or("manifest: bad window_end")?;
    Ok(Window { ws, we, cws: ws - WEEK_MS })
}

pub struct Products {
    pub map: StrMap,
    /// Category names, sorted byte-wise; a category's index is its rank.
    pub cats: Vec<&'static [u8]>,
    pub unknown: u32,
    /// Per product index: category rank. `map` values index this.
    pub cat_of: Vec<u32>,
}

pub fn read_products(dir: &Path) -> Result<Products, String> {
    let data = open_view(&dir.join("products.csv")).map_err(|e| format!("products.csv: {e}"))?;
    let mut rows: Vec<(&'static [u8], &'static [u8])> = Vec::new();
    for_each_line(data, first_row(data), data.len(), |l| {
        let mut f = [(0, 0); 2];
        if split::<2>(l, &mut f) {
            rows.push((&l[f[0].0..f[0].1], &l[f[1].0..f[1].1]));
        }
    });
    let mut cats: Vec<&'static [u8]> = rows.iter().map(|r| r.1).collect();
    cats.push(b"unknown");
    cats.sort_unstable();
    cats.dedup();
    let unknown = cats.binary_search(&&b"unknown"[..]).unwrap() as u32;
    let mut map = StrMap::with_capacity(rows.len());
    let mut cat_of = Vec::with_capacity(rows.len());
    for (pid, cat) in rows {
        let rank = cats.binary_search(&cat).unwrap() as u32;
        if map.insert(pid, cat_of.len() as u32) {
            cat_of.push(rank);
        }
    }
    Ok(Products { map, cats, unknown, cat_of })
}

pub struct Campaigns {
    pub map: StrMap,
    /// (campaign_id, channel); the last entry is `organic`.
    pub rows: Vec<(&'static [u8], &'static [u8])>,
}

impl Campaigns {
    pub fn organic(&self) -> u32 {
        (self.rows.len() - 1) as u32
    }
}

pub fn read_campaigns(dir: &Path) -> Result<Campaigns, String> {
    let data = open_view(&dir.join("campaigns.csv")).map_err(|e| format!("campaigns.csv: {e}"))?;
    let mut raw: Vec<(&'static [u8], &'static [u8])> = Vec::new();
    for_each_line(data, first_row(data), data.len(), |l| {
        let mut f = [(0, 0); 2];
        if split::<2>(l, &mut f) {
            raw.push((&l[f[0].0..f[0].1], &l[f[1].0..f[1].1]));
        }
    });
    let mut map = StrMap::with_capacity(raw.len() + 1);
    let mut rows = Vec::with_capacity(raw.len() + 1);
    for (id, ch) in raw {
        if map.insert(id, rows.len() as u32) {
            rows.push((id, ch));
        }
    }
    rows.push((&b"organic"[..], &b"organic"[..]));
    Ok(Campaigns { map, rows })
}
