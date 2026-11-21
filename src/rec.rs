//! Fixed-size records moved between stages, and the shared read-only context.

use crate::links::LinkTable;
use crate::tables::{Campaigns, Products, Window};
use crate::util::{field_at, hash128};

/// A parsed event line before dedup. `ep` holds the address of the event_id bytes
/// (bits 0..47) and the "survives if kept" flag (bit 63).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ev {
    pub eh: u64,
    pub ep: u64,
    pub ing: i64,
    pub ts: i64,
    pub ulo: u64,
    pub uhi: u64,
    pub uptr: u64,
    /// price_cents when the event is a valid purchase, else 0.
    pub price: u32,
    pub cat: u32,
}

pub const OK_BIT: u64 = 1 << 63;
pub const PTR_MASK: u64 = (1 << 48) - 1;

/// A surviving event, tagged with its person.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SEv {
    pub lo: u64,
    pub hi: u64,
    pub ts: i64,
    /// Name reference, see `Ctx::name`.
    pub name: u64,
    pub price: u32,
    pub cat: u32,
}

/// A valid click, tagged with its person.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SClick {
    pub lo: u64,
    pub hi: u64,
    pub ts: i64,
    /// Address of the click_id bytes.
    pub cid: u64,
    pub camp: u32,
    pub _pad: u32,
}

const _: () = assert!(std::mem::size_of::<Ev>() == 64);
const _: () = assert!(std::mem::size_of::<SEv>() == 40);
const _: () = assert!(std::mem::size_of::<SClick>() == 40);

pub const NAME_LINKED: u64 = 1 << 63;

pub struct Ctx {
    pub win: Window,
    pub prods: Products,
    pub camps: Campaigns,
    pub links: LinkTable,
    pub es_bits: u32,
    pub ps_bits: u32,
    pub threads: usize,
}

impl Ctx {
    #[inline(always)]
    pub fn eshard(&self, eh: u64) -> usize {
        (eh >> (64 - self.es_bits)) as usize
    }
    #[inline(always)]
    pub fn pshard(&self, lo: u64) -> usize {
        (lo >> (64 - self.ps_bits)) as usize
    }
    /// Person key and name reference for a user-ID hash and the address of its bytes.
    #[inline(always)]
    pub fn person(&self, ulo: u64, uhi: u64, uptr: u64) -> (u64, u64, u64) {
        match self.links.get(ulo, uhi) {
            None => (ulo, uhi, uptr),
            Some(c) => {
                let comp = &self.links.comps[c as usize];
                (comp.lo, comp.hi, NAME_LINKED | c as u64)
            }
        }
    }
    pub fn name(&self, name: u64) -> &'static [u8] {
        if name & NAME_LINKED != 0 {
            self.links.comps[(name & !NAME_LINKED) as usize].bytes()
        } else {
            unsafe { field_at(name) }
        }
    }
}

#[allow(dead_code)]
pub fn user_hash(s: &[u8]) -> (u64, u64) {
    hash128(s)
}
