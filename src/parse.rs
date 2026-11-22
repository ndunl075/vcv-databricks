//! Turns rows into fixed-size records, one input piece at a time.
//!
//! Events: every row with an event_id becomes an `Ev`, valid or not, because an invalid copy can
//! still be the one dedup keeps. Each event_id also marks a 2-bit cell chosen by its hash:
//! "seen" and "seen more than once". Only events whose cell says "more than once" need the exact
//! dedup later. The marks are made after reading, not during it: each reader files its events'
//! hash tags by cell range, then each range is marked by one thread alone, with plain writes to
//! memory that stays in cache, instead of a locked instruction on a far cache line per row.
//! Clicks: only valid clicks become a `Ck`. Links: both IDs' numbers.
//!
//! User IDs become numbers through `Names`. Those lookups touch memory far apart, so each row's lookups are started (address computed, cache line requested) and
//! finished a few rows later, when the memory has arrived.

use crate::ids::{hash_of, word, Key, Names, Supply, Table, LOW_BYTES};
use crate::os;
use crate::scan::{rows_in, rows_wide, Index, Lens, Plain, Rows, SlowSqueeze, Squeeze, Wide};
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
    /// Marks the cells of every tag in `lists`. All the tags must be in one range (see
    /// `range_of`), and no other thread may mark that range at the same time: the words are
    /// changed with plain reads and writes.
    pub fn mark_range(&self, lists: &[&[u32]]) {
        const AHEAD: usize = 16;
        for tags in lists {
            for (i, &tag) in tags.iter().enumerate() {
                if let Some(&next) = tags.get(i + AHEAD) {
                    os::soon(self.word_of(next));
                }
                let sh = ((tag >> self.shift) & 31) * 2;
                // SAFETY: see `word_of`; this thread alone touches the range.
                let w = unsafe { &*self.word_of(tag) };
                let now = w.load(Relaxed);
                let bit = if (now >> sh) & 1 != 0 { 2u64 } else { 1 };
                w.store(now | (bit << sh), Relaxed);
            }
        }
    }
}

/// Tags are filed into this many ranges of cells; each range is marked by one thread.
pub const RANGES: usize = 256;

/// One reader's event tags, filed by cell range: a fixed bin per range in one region of memory,
/// and a list for whatever does not fit.
pub struct TagBins {
    room: usize,
    bins: *mut u32,
    lens: Vec<usize>,
    spill: Vec<Vec<u32>>,
}

// SAFETY: the region is owned by this value alone, and shared only for reading.
unsafe impl Send for TagBins {}
unsafe impl Sync for TagBins {}

impl TagBins {
    /// Bins for about `rows` events.
    pub fn new(rows: usize) -> TagBins {
        let room = (rows / RANGES + rows / RANGES / 8 + 16).next_multiple_of(16);
        TagBins { room, bins: os::zeroed_array::<u32>(room * RANGES), lens: vec![0; RANGES], spill: (0..RANGES).map(|_| Vec::new()).collect() }
    }
    #[inline(always)]
    pub fn push(&mut self, tag: u32) {
        let r = range_of(tag);
        // SAFETY: `r` is below RANGES; the bin index is checked against its room.
        unsafe {
            let n = self.lens.get_unchecked_mut(r);
            if *n < self.room {
                *self.bins.add(r * self.room + *n) = tag;
                *n += 1;
            } else {
                self.spill.get_unchecked_mut(r).push(tag);
            }
        }
    }
    /// The tags of range `r`, in up to two slices.
    pub fn range(&self, r: usize) -> [&[u32]; 2] {
        // SAFETY: the first `lens[r]` items of bin `r` were written.
        let bin = unsafe { std::slice::from_raw_parts(self.bins.add(r * self.room), self.lens[r]) };
        [bin, &self.spill[r][..]]
    }
}

/// The range of cells `tag` belongs to. Ranges never share a word: the cell count is at least
/// 2^16, so a word (32 cells) never spans a change in the tag's top 8 bits.
#[inline(always)]
pub fn range_of(tag: u32) -> usize {
    (tag >> 24) as usize
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
    wx: Wide,
    dates: Dates,
    supply: Supply,
    ev_free: *mut Ev,
    ev_left: usize,
    pub ev_runs: Vec<Run<Ev>>,
    ck_free: *mut Ck,
    ck_left: usize,
    pub ck_runs: Vec<Run<Ck>>,
    /// Event hash tags, filed by cell range (`range_of`).
    pub tags: TagBins,
}

impl Reader {
    /// A reader that expects to see about `events` event rows.
    pub fn new(events: usize) -> Reader {
        Reader {
            ix: Index::new(),
            wx: Wide::new(),
            dates: Dates::new(),
            supply: Supply::new(),
            ev_free: std::ptr::null_mut(),
            ev_left: 0,
            ev_runs: Vec::new(),
            ck_free: std::ptr::null_mut(),
            ck_left: 0,
            ck_runs: Vec::new(),
            tags: TagBins::new(events),
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
        let idle = Pending { into: std::ptr::null_mut(), key: [0; 3], hash: 0, at: std::ptr::null(), len: 0 };
        Flight { sh, table, guard: Some(guard), ring: [idle; LAG], pushed: 0 }
    }

    #[inline(always)]
    unsafe fn finish(&self, supply: &mut Supply, p: &Pending) {
        if !p.into.is_null() {
            *p.into = self.table.finish(self.sh.names, supply, &p.key, p.hash, p.at, p.len);
        }
    }

    #[inline(always)]
    unsafe fn push(&mut self, supply: &mut Supply, p: Pending) {
        let slot = self.ring.as_mut_ptr().add(self.pushed & (LAG - 1));
        if self.pushed >= LAG {
            self.finish(supply, &*slot);
        }
        slot.write(p);
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
pub const SKIP: u32 = 0;

const VIEW: u64 = u64::from_le_bytes(*b"view\0\0\0\0");
const CART: u64 = u64::from_le_bytes(*b"cart\0\0\0\0");
const PURCHASE: u64 = u64::from_le_bytes(*b"purchase");

struct EventRows<'a, 'b> {
    fl: Flight<'a>,
    data: *const u8,
    dates: &'b mut Dates,
    supply: &'b mut Supply,
    runs: &'b mut Vec<Run<Ev>>,
    tags: &'b mut TagBins,
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
    unsafe fn fields(&mut self, from: usize, a: &[usize; 8], e: &[usize; 8]) {
        if SKIP & 1 != 0 {
            return;
        }
        let d = self.data;
        let (a0, a1, a2, a3, a4, a5, a6) = (a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
        let (e0, e1, e2, e3, e4, e5, e6) = (e[0], e[1], e[2], e[3], e[4], e[5], e[6]);
        if (a0 == e0) | (SKIP & 2 != 0) {
            return;
        }
        let sh = self.fl.sh;
        let tag = (hash_of(d.add(a0), e0 - a0) >> 32) as u32;
        let user_len = e1 - a1;
        let (user_key, user_hash) = self.fl.table.begin(d.add(a1), user_len);

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
        self.tags.push(tag);
        let into = if ok { std::ptr::addr_of_mut!((*rec).who) } else { std::ptr::null_mut() };
        if SKIP & 8 != 0 {
            return;
        }
        self.fl.push(self.supply, Pending { into, key: user_key, hash: user_hash, at: d.add(a1), len: user_len });
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
    unsafe fn fields(&mut self, _from: usize, a: &[usize; 8], e: &[usize; 8]) {
        let d = self.data;
        let (a0, a1, a2, a3) = (a[0], a[1], a[2], a[3]);
        let (e0, e1, e2, e3) = (e[0], e[1], e[2], e[3]);
        if (a0 == e0) | (a1 == e1) {
            return;
        }
        let sh = self.fl.sh;
        let user_len = e1 - a1;
        let (user_key, user_hash) = self.fl.table.begin(d.add(a1), user_len);
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
        self.fl.push(self.supply, Pending { into: std::ptr::addr_of_mut!((*rec).who), key: user_key, hash: user_hash, at: d.add(a1), len: user_len });
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
    unsafe fn fields(&mut self, _from: usize, a: &[usize; 8], e: &[usize; 8]) {
        let d = self.data;
        let (a0, a1, e0, e1) = (a[0], a[1], e[0], e[1]);
        if (a0 == e0) | (a1 == e1) {
            return;
        }
        let n = self.pairs.len();
        let slot = self.pairs.as_mut_ptr().add(n) as *mut u32;
        self.pairs.set_len(n + 1);
        for (k, (a, e)) in [(a0, e0), (a1, e1)].into_iter().enumerate() {
            let (key, hash) = self.fl.table.begin(d.add(a), e - a);
            self.fl.push(self.supply, Pending { into: slot.add(k), key, hash, at: d.add(a), len: e - a });
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

/// How rows are found: the general scanner, or the one built around `pext`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scanner {
    General,
    Wide,
}

#[inline(always)]
unsafe fn scan<L: Lens, Q: Squeeze, R: Rows<N>, const N: usize>(wide: bool, ix: &mut Index, wx: &mut Wide, p: &Piece, rows: &mut R) {
    let data = p.data.as_ptr();
    let open_end = p.to == p.data.len() && p.data[p.to - 1] != b'\n';
    if wide {
        rows_wide::<L, Q, R, N>(ix, wx, data, p.from, p.to, open_end, rows);
    } else {
        rows_in::<L, R, N>(ix, data, p.from, p.to, open_end, rows);
    }
}

#[inline(always)]
unsafe fn read_with<L: Lens, Q: Squeeze>(sh: &Shared, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece, wide: bool) {
    if p.from >= p.to {
        return;
    }
    let data = p.data.as_ptr();
    match p.kind {
        Kind::Events => {
            let mut rows = EventRows {
                fl: Flight::new(sh),
                data,
                dates: &mut r.dates,
                supply: &mut r.supply,
                runs: &mut r.ev_runs,
                tags: &mut r.tags,
                free: r.ev_free,
                left: r.ev_left,
                run_first: r.ev_free,
                run_base: p.from,
            };
            scan::<L, Q, _, 6>(wide, &mut r.ix, &mut r.wx, p, &mut rows);
            rows.fl.close(rows.supply);
            rows.end_run();
            let (free, left) = (rows.free, rows.left);
            r.ev_free = free;
            r.ev_left = left;
        }
        Kind::Clicks => {
            let mut rows = ClickRows { fl: Flight::new(sh), data, dates: &mut r.dates, supply: &mut r.supply, runs: &mut r.ck_runs, free: r.ck_free, left: r.ck_left, run_first: r.ck_free };
            scan::<L, Q, _, 3>(wide, &mut r.ix, &mut r.wx, p, &mut rows);
            rows.fl.close(rows.supply);
            rows.end_run();
            let (free, left) = (rows.free, rows.left);
            r.ck_free = free;
            r.ck_left = left;
        }
        Kind::Links => {
            let mut rows = LinkRows { fl: Flight::new(sh), data, supply: &mut r.supply, pairs };
            scan::<L, Q, _, 1>(wide, &mut r.ix, &mut r.wx, p, &mut rows);
            rows.fl.close(rows.supply);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,popcnt")]
unsafe fn read_fast(sh: &Shared, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece) {
    read_with::<crate::scan::Avx, crate::scan::Bmi2>(sh, r, pairs, p, true)
}

/// True when this CPU can run the fast build of the reader (AVX2, BMI1, BMI2, POPCNT).
pub fn fast_cpu() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("bmi1") && is_x86_feature_detected!("bmi2") && is_x86_feature_detected!("popcnt")
    }
    #[cfg(not(target_arch = "x86_64"))]
    false
}

/// Reads one piece. `fast` (only after `fast_cpu()` said yes) runs the AVX2 + BMI2 build of the
/// wide scanner; otherwise `scanner` picks the algorithm for the build every CPU can run.
pub fn read(sh: &Shared, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece, fast: bool, scanner: Scanner) {
    // SAFETY: pieces lie inside mapped files, which carry a readable zero tail.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        if fast {
            return read_fast(sh, r, pairs, p);
        }
        let _ = fast;
        read_with::<Plain, SlowSqueeze>(sh, r, pairs, p, scanner == Scanner::Wide)
    }
}

/// Counts rows; used to time a scanner on its own.
struct CountRows(usize);

impl<const N: usize> Rows<N> for CountRows {
    #[inline(always)]
    unsafe fn window(&mut self, _rows: usize) {}
    #[inline(always)]
    unsafe fn fields(&mut self, from: usize, a: &[usize; 8], e: &[usize; 8]) {
        self.0 += 1 + ((from ^ a[6] ^ e[6]) & 0);
    }
}

#[inline(always)]
unsafe fn layer_with<L: Lens, Q: Squeeze>(what: u32, r: &mut Reader, data: &'static [u8], from: usize, to: usize) -> usize {
    let d = data.as_ptr();
    match what {
        0 => {
            // Every byte read once, eight at a time.
            let mut sum = 0u64;
            let mut p = from;
            while p + 8 <= to {
                sum = sum.wrapping_add(word(d.add(p)));
                p += 8;
            }
            sum as usize
        }
        1 => {
            let mut n = 0usize;
            let mut p = from;
            while p + 64 <= to {
                let m = L::marks(d.add(p));
                n += (m.line.count_ones() + (m.comma & m.quote).count_ones()) as usize;
                p += 64;
            }
            n
        }
        2 => {
            let mut rows = CountRows(0);
            rows_in::<L, _, 6>(&mut r.ix, d, from, to, false, &mut rows);
            rows.0
        }
        _ => {
            let mut rows = CountRows(0);
            rows_wide::<L, Q, _, 6>(&mut r.ix, &mut r.wx, d, from, to, false, &mut rows);
            rows.0
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,popcnt")]
unsafe fn layer_fast(what: u32, r: &mut Reader, data: &'static [u8], from: usize, to: usize) -> usize {
    layer_with::<crate::scan::Avx, crate::scan::Bmi2>(what, r, data, from, to)
}

/// One layer of the reader over `data[from..to]` (whole rows), for timing experiments:
/// 0 = touch every byte, 1 = classify every block, 2 = general scanner, 3 = wide scanner.
pub fn layer(what: u32, r: &mut Reader, data: &'static [u8], from: usize, to: usize, fast: bool) -> usize {
    // SAFETY: as `read`.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        if fast {
            return layer_fast(what, r, data, from, to);
        }
        let _ = fast;
        layer_with::<Plain, SlowSqueeze>(what, r, data, from, to)
    }
}
