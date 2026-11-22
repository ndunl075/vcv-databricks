//! Turns rows into fixed-size records, one input piece at a time.
//!
//! Events: every row with an event_id becomes an `Ev`, valid or not, because an invalid copy can
//! still be the one dedup keeps. Each event_id also marks a 2-bit cell chosen by its hash:
//! "seen" and "seen more than once". Only events whose cell says "more than once" need the exact
//! dedup later.
//! Clicks: only valid clicks become a `Ck`. Links: both IDs' numbers.
//!
//! User IDs become numbers through `Names`. Those lookups, and the cell updates, touch memory
//! far apart, so each row's lookups are started (address computed, cache line requested) and
//! finished a few rows later, when the memory has arrived.

use crate::ids::{key_of, spread, word, Key, Names, Supply, Table, LOW_BYTES};
use crate::os;
use crate::scan::{rows_in, Fields, Index, Lens, Plain, Rows};
use crate::stamp::{Dates, NEVER};
use crate::tables::{Campaigns, Products, Window};
use crate::ts::price_cents;
use crate::util::DAY_MS;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::RwLockReadGuard;

/// `who` of an event that takes no part in sessions: invalid, or a copy that lost dedup.
pub const NOBODY: u32 = u32::MAX;

/// One event row.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ev {
    /// Valid events: milliseconds after the window start, shifted left 24, plus the price of a
    /// valid purchase (0 for views and carts).
    pub key: u64,
    /// User number, later the person's seat; `NOBODY` when the event is not valid.
    pub who: u32,
    /// Category rank of a purchase.
    pub cat: u32,
    /// High half of the event_id hash.
    pub tag: u32,
    /// Where the row starts, counted from its run's base.
    pub at: u32,
}

/// One valid click.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ck {
    /// Milliseconds after the click window start.
    pub when: u64,
    pub who: u32,
    pub camp: u32,
    /// First byte of the click_id.
    pub id_at: *const u8,
}

const _: () = assert!(std::mem::size_of::<Ev>() == 24 && std::mem::size_of::<Ck>() == 24);

/// A stretch of records written one after another.
pub struct Run<T> {
    pub first: *mut T,
    pub n: usize,
    /// Events: the address `Ev::at` counts from.
    pub base: *const u8,
}

/// The 2-bit cells for event_id hashes.
pub struct Cells {
    pub words: *const AtomicU64,
    /// `tag >> shift` is the cell number.
    pub shift: u32,
}

// SAFETY: the words are atomics in memory that is never freed.
unsafe impl Sync for Cells {}

impl Cells {
    pub fn new(event_bytes: usize) -> Cells {
        // About a dozen cells per row keeps unrelated IDs from sharing a cell too often.
        let cells = (event_bytes / 3).max(1 << 16).next_power_of_two().min(1 << 32);
        let cells = if cells > event_bytes / 3 && cells > 1 << 16 { cells / 2 } else { cells };
        let bits = cells.trailing_zeros();
        Cells { words: os::zeroed_array::<AtomicU64>(cells / 32), shift: 32 - bits }
    }
    #[inline(always)]
    pub fn word_of(&self, tag: u32) -> *const AtomicU64 {
        // SAFETY: the cell number is below the cell count.
        unsafe { self.words.add(((tag >> self.shift) >> 5) as usize) }
    }
    /// True when at least two rows marked this tag's cell.
    #[inline(always)]
    pub fn crowded(&self, tag: u32) -> bool {
        let cell = tag >> self.shift;
        // SAFETY: see `word_of`.
        let w = unsafe { (*self.word_of(tag)).load(Relaxed) };
        (w >> ((cell & 31) * 2)) & 2 != 0
    }
    #[inline(always)]
    fn mark(&self, tag: u32) {
        let cell = tag >> self.shift;
        let sh = (cell & 31) * 2;
        // SAFETY: see `word_of`.
        let w = unsafe { &*self.word_of(tag) };
        let before = w.fetch_or(1 << sh, Relaxed);
        if (before >> sh) & 3 == 1 {
            w.fetch_or(2 << sh, Relaxed);
        }
    }
}

/// Everything the row readers share.
pub struct Shared<'a> {
    pub win: Window,
    pub prods: &'a Products,
    pub camps: &'a Campaigns,
    pub names: &'a Names,
    pub cells: Cells,
}

// SAFETY: the raw pointers inside lead to memory that is only used through atomics.
unsafe impl Sync for Shared<'_> {}

/// Records per block of a thread's record store.
const BLOCK: usize = (8 << 20) / 24;

/// One thread's scratch and output for the reading stage.
pub struct Reader {
    ix: Index,
    dates: Dates,
    supply: Supply,
    ev_free: *mut Ev,
    ev_left: usize,
    pub ev_runs: Vec<Run<Ev>>,
    ck_free: *mut Ck,
    ck_left: usize,
    pub ck_runs: Vec<Run<Ck>>,
}

impl Reader {
    pub fn new() -> Reader {
        Reader {
            ix: Index::new(),
            dates: Dates::new(),
            supply: Supply::new(),
            ev_free: std::ptr::null_mut(),
            ev_left: 0,
            ev_runs: Vec::new(),
            ck_free: std::ptr::null_mut(),
            ck_left: 0,
            ck_runs: Vec::new(),
        }
    }
}

/// A lookup that has been started.
#[derive(Clone, Copy)]
struct Pending {
    /// Where the user number goes; null when there is none to find.
    into: *mut u32,
    key: Key,
    hash: u64,
    at: *const u8,
    len: usize,
    /// Event tag whose cell to mark, when `marks` is set.
    tag: u32,
    marks: bool,
}

const LAG: usize = 8;

/// The lookups in flight, and the table they run against.
struct Flight<'a> {
    sh: &'a Shared<'a>,
    table: Table,
    guard: Option<RwLockReadGuard<'a, ()>>,
    ring: [Pending; LAG],
    pushed: usize,
}

impl<'a> Flight<'a> {
    fn new(sh: &'a Shared<'a>) -> Flight<'a> {
        let guard = sh.names.gate.read().unwrap();
        let table = sh.names.table();
        let idle = Pending { into: std::ptr::null_mut(), key: [0; 3], hash: 0, at: std::ptr::null(), len: 0, tag: 0, marks: false };
        Flight { sh, table, guard: Some(guard), ring: [idle; LAG], pushed: 0 }
    }

    #[inline(always)]
    unsafe fn finish(&self, supply: &mut Supply, p: &Pending) {
        if p.marks {
            self.sh.cells.mark(p.tag);
        }
        if !p.into.is_null() {
            *p.into = self.table.number(self.sh.names, supply, &p.key, p.hash, p.at, p.len);
        }
    }

    #[inline(always)]
    unsafe fn push(&mut self, supply: &mut Supply, p: Pending) {
        let slot = self.pushed & (LAG - 1);
        if self.pushed >= LAG {
            let old = *self.ring.get_unchecked(slot);
            self.finish(supply, &old);
        }
        *self.ring.get_unchecked_mut(slot) = p;
        self.pushed += 1;
    }

    unsafe fn land(&mut self, supply: &mut Supply) {
        let n = self.pushed.min(LAG);
        for k in 0..n {
            let p = self.ring[(self.pushed - n + k) & (LAG - 1)];
            self.finish(supply, &p);
        }
        self.pushed = 0;
    }

    /// Between windows: report new IDs, and step aside if the table has to grow.
    #[inline(always)]
    unsafe fn checkpoint(&mut self, supply: &mut Supply) {
        if self.sh.names.report(supply, &self.table) {
            self.step_aside(supply);
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn step_aside(&mut self, supply: &mut Supply) {
        self.land(supply);
        self.guard = None;
        self.sh.names.grow();
        self.guard = Some(self.sh.names.gate.read().unwrap());
        self.table = self.sh.names.table();
    }

    unsafe fn close(&mut self, supply: &mut Supply) {
        self.land(supply);
        self.sh.names.report(supply, &self.table);
        self.guard = None;
    }
}

/// Experiment switches for timing parts of the reader; 0 in every real build.
pub const SKIP: u32 = 8;

const VIEW: u64 = u64::from_le_bytes(*b"view\0\0\0\0");
const CART: u64 = u64::from_le_bytes(*b"cart\0\0\0\0");
const PURCHASE: u64 = u64::from_le_bytes(*b"purchase");

struct EventRows<'a, 'b> {
    fl: Flight<'a>,
    data: *const u8,
    dates: &'b mut Dates,
    supply: &'b mut Supply,
    runs: &'b mut Vec<Run<Ev>>,
    free: *mut Ev,
    left: usize,
    run_first: *mut Ev,
    /// Offset in `data` that `Ev::at` counts from.
    run_base: usize,
}

impl EventRows<'_, '_> {
    fn end_run(&mut self) {
        // SAFETY: both pointers are in the same block.
        let n = unsafe { self.free.offset_from(self.run_first) } as usize;
        if n > 0 {
            self.runs.push(Run { first: self.run_first, n, base: unsafe { self.data.add(self.run_base) } });
        }
        self.run_first = self.free;
    }
}

impl Rows<6> for EventRows<'_, '_> {
    #[inline(always)]
    unsafe fn window(&mut self, rows: usize) {
        self.fl.checkpoint(self.supply);
        if self.left < rows {
            self.end_run();
            self.free = os::zeroed_array::<Ev>(BLOCK);
            self.left = BLOCK;
            self.run_first = self.free;
        }
    }

    #[inline(always)]
    unsafe fn row(&mut self, from: usize, c: &[usize; 6], to: usize, quotes: u32) {
        if SKIP & 1 != 0 {
            return;
        }
        let d = self.data;
        let mut f = Fields::new();
        let (mut a0, mut e0) = (from, c[0]);
        let (mut a1, mut e1) = (c[0] + 1, c[1]);
        let (mut a2, mut e2) = (c[1] + 1, c[2]);
        let (mut a3, mut e3) = (c[2] + 1, c[3]);
        let (mut a4, mut e4) = (c[3] + 1, c[4]);
        let (mut a5, mut e5) = (c[4] + 1, c[5]);
        let (mut a6, mut e6) = (c[5] + 1, to);
        f.strip(d, &mut a0, &mut e0);
        f.strip(d, &mut a1, &mut e1);
        f.strip(d, &mut a2, &mut e2);
        f.strip(d, &mut a3, &mut e3);
        f.strip(d, &mut a4, &mut e4);
        f.strip(d, &mut a5, &mut e5);
        f.strip(d, &mut a6, &mut e6);
        if !f.sound(quotes) | (a0 == e0) | (SKIP & 2 != 0) {
            return;
        }
        let sh = self.fl.sh;
        let tag = (spread(&key_of(d.add(a0), e0 - a0)) >> 32) as u32;
        os::soon(sh.cells.word_of(tag));
        let user_len = e1 - a1;
        let user_key = key_of(d.add(a1), user_len);
        let user_hash = spread(&user_key);
        self.fl.table.warm(user_hash);

        let (ts, ingest) = if SKIP & 4 != 0 { (sh.win.ws + 5, sh.win.ws + 6) } else { (self.dates.instant(d.add(a2), e2 - a2), self.dates.instant(d.add(a3), e3 - a3)) };
        let mut ok = (user_len != 0) & (a5 != e5) & (ts >= sh.win.ws) & (ts < sh.win.we) & (ingest != NEVER);
        ok &= ingest.saturating_sub(ts) <= DAY_MS;
        let kind_len = e4 - a4;
        let kind = word(d.add(a4)) & *LOW_BYTES.get_unchecked(kind_len.min(8));
        let (mut price, mut cat) = (0u32, 0u32);
        if (kind_len == 4) & ((kind == VIEW) | (kind == CART)) {
        } else if (kind_len == 8) & (kind == PURCHASE) {
            match price_cents(std::slice::from_raw_parts(d.add(a6), e6 - a6)) {
                Some(p) if ok => {
                    price = p;
                    cat = sh.prods.category(std::slice::from_raw_parts(d.add(a5), e5 - a5));
                }
                Some(_) => {}
                None => ok = false,
            }
        } else {
            ok = false;
        }

        let mut at = from - self.run_base;
        if at > u32::MAX as usize {
            self.end_run();
            self.run_base = from;
            at = 0;
        }
        let rec = self.free;
        self.free = rec.add(1);
        self.left -= 1;
        let key = if ok { (((ts - sh.win.ws) as u64) << 24) | price as u64 } else { 0 };
        rec.write(Ev { key, who: NOBODY, cat, tag, at: at as u32 });
        let into = if ok { std::ptr::addr_of_mut!((*rec).who) } else { std::ptr::null_mut() };
        if SKIP & 8 != 0 {
            return;
        }
        self.fl.push(self.supply, Pending { into, key: user_key, hash: user_hash, at: d.add(a1), len: user_len, tag, marks: true });
    }
}

struct ClickRows<'a, 'b> {
    fl: Flight<'a>,
    data: *const u8,
    dates: &'b mut Dates,
    supply: &'b mut Supply,
    runs: &'b mut Vec<Run<Ck>>,
    free: *mut Ck,
    left: usize,
    run_first: *mut Ck,
}

impl ClickRows<'_, '_> {
    fn end_run(&mut self) {
        // SAFETY: both pointers are in the same block.
        let n = unsafe { self.free.offset_from(self.run_first) } as usize;
        if n > 0 {
            self.runs.push(Run { first: self.run_first, n, base: self.data });
        }
        self.run_first = self.free;
    }
}

impl Rows<3> for ClickRows<'_, '_> {
    #[inline(always)]
    unsafe fn window(&mut self, rows: usize) {
        self.fl.checkpoint(self.supply);
        if self.left < rows {
            self.end_run();
            self.free = os::zeroed_array::<Ck>(BLOCK);
            self.left = BLOCK;
            self.run_first = self.free;
        }
    }

    #[inline(always)]
    unsafe fn row(&mut self, from: usize, c: &[usize; 3], to: usize, quotes: u32) {
        let d = self.data;
        let mut f = Fields::new();
        let (mut a0, mut e0) = (from, c[0]);
        let (mut a1, mut e1) = (c[0] + 1, c[1]);
        let (mut a2, mut e2) = (c[1] + 1, c[2]);
        let (mut a3, mut e3) = (c[2] + 1, to);
        f.strip(d, &mut a0, &mut e0);
        f.strip(d, &mut a1, &mut e1);
        f.strip(d, &mut a2, &mut e2);
        f.strip(d, &mut a3, &mut e3);
        if !f.sound(quotes) | (a0 == e0) | (a1 == e1) {
            return;
        }
        let sh = self.fl.sh;
        let user_len = e1 - a1;
        let user_key = key_of(d.add(a1), user_len);
        let user_hash = spread(&user_key);
        self.fl.table.warm(user_hash);
        let ts = self.dates.instant(d.add(a3), e3 - a3);
        if (ts < sh.win.cws) | (ts >= sh.win.we) {
            return;
        }
        let camp = match sh.camps.find(std::slice::from_raw_parts(d.add(a2), e2 - a2)) {
            Some(c) => c,
            None => return,
        };
        let rec = self.free;
        self.free = rec.add(1);
        self.left -= 1;
        rec.write(Ck { when: (ts - sh.win.cws) as u64, who: NOBODY, camp, id_at: d.add(a0) });
        self.fl.push(self.supply, Pending { into: std::ptr::addr_of_mut!((*rec).who), key: user_key, hash: user_hash, at: d.add(a1), len: user_len, tag: 0, marks: false });
    }
}

struct LinkRows<'a, 'b> {
    fl: Flight<'a>,
    data: *const u8,
    supply: &'b mut Supply,
    pairs: &'b mut Vec<[u32; 2]>,
}

impl Rows<1> for LinkRows<'_, '_> {
    #[inline(always)]
    unsafe fn window(&mut self, rows: usize) {
        self.fl.checkpoint(self.supply);
        if self.pairs.capacity() - self.pairs.len() < rows {
            // Lookups in flight point into the list; finish them before it moves.
            self.fl.land(self.supply);
            self.pairs.reserve(rows.max(self.pairs.len()));
        }
    }

    #[inline(always)]
    unsafe fn row(&mut self, from: usize, c: &[usize; 1], to: usize, quotes: u32) {
        let d = self.data;
        let mut f = Fields::new();
        let (mut a0, mut e0) = (from, c[0]);
        let (mut a1, mut e1) = (c[0] + 1, to);
        f.strip(d, &mut a0, &mut e0);
        f.strip(d, &mut a1, &mut e1);
        if !f.sound(quotes) | (a0 == e0) | (a1 == e1) {
            return;
        }
        let n = self.pairs.len();
        let slot = self.pairs.as_mut_ptr().add(n) as *mut u32;
        self.pairs.set_len(n + 1);
        for (k, (a, e)) in [(a0, e0), (a1, e1)].into_iter().enumerate() {
            let key = key_of(d.add(a), e - a);
            let hash = spread(&key);
            self.fl.table.warm(hash);
            self.fl.push(self.supply, Pending { into: slot.add(k), key, hash, at: d.add(a), len: e - a, tag: 0, marks: false });
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Links,
    Events,
    Clicks,
}

/// Rows `from..to` of one file.
#[derive(Clone, Copy)]
pub struct Piece {
    pub kind: Kind,
    pub data: &'static [u8],
    pub from: usize,
    pub to: usize,
}

#[inline(always)]
unsafe fn read_with<L: Lens>(sh: &Shared, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece) {
    if p.from >= p.to {
        return;
    }
    let data = p.data.as_ptr();
    let open_end = p.to == p.data.len() && p.data[p.to - 1] != b'\n';
    match p.kind {
        Kind::Events => {
            let mut rows = EventRows {
                fl: Flight::new(sh),
                data,
                dates: &mut r.dates,
                supply: &mut r.supply,
                runs: &mut r.ev_runs,
                free: r.ev_free,
                left: r.ev_left,
                run_first: r.ev_free,
                run_base: p.from,
            };
            rows_in::<L, _, 6>(&mut r.ix, data, p.from, p.to, open_end, &mut rows);
            rows.fl.close(rows.supply);
            rows.end_run();
            r.ev_free = rows.free;
            r.ev_left = rows.left;
        }
        Kind::Clicks => {
            let mut rows = ClickRows { fl: Flight::new(sh), data, dates: &mut r.dates, supply: &mut r.supply, runs: &mut r.ck_runs, free: r.ck_free, left: r.ck_left, run_first: r.ck_free };
            rows_in::<L, _, 3>(&mut r.ix, data, p.from, p.to, open_end, &mut rows);
            rows.fl.close(rows.supply);
            rows.end_run();
            r.ck_free = rows.free;
            r.ck_left = rows.left;
        }
        Kind::Links => {
            let mut rows = LinkRows { fl: Flight::new(sh), data, supply: &mut r.supply, pairs };
            rows_in::<L, _, 1>(&mut r.ix, data, p.from, p.to, open_end, &mut rows);
            rows.fl.close(rows.supply);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
unsafe fn read_wide(sh: &Shared, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece) {
    read_with::<crate::scan::Avx>(sh, r, pairs, p)
}

/// Reads one piece. `wide` selects the AVX2 build of the reader; the caller has checked the CPU.
pub fn read(sh: &Shared, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece, wide: bool) {
    // SAFETY: pieces lie inside mapped files, which carry a readable zero tail.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        if wide {
            return read_wide(sh, r, pairs, p);
        }
        let _ = wide;
        read_with::<Plain>(sh, r, pairs, p)
    }
}
