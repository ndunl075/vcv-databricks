//! Turns rows into fixed-size records, one input piece at a time.
//!
//! Events: links are read first, so when an event is read its person is known, and a valid event
//! is dealt into its person's shard at once (`shard::Deal`). Every row with an event_id, valid
//! or not, is also filed as a `Sighting` (an invalid copy can still be the one dedup keeps), and
//! its event_id marks a 2-bit cell chosen by its hash: "seen" and "seen more than once". Only
//! events whose cell says "more than once" need the exact dedup (`dedup::settle`), which flags
//! the dealt rows of the copies that lose. The marks are made after reading: each reader files
//! its sightings by cell range, then each range is marked and settled by one thread alone, in
//! memory that stays in cache.
//! Clicks: only valid clicks become a `Ck`. Links: both IDs' numbers.
//!
//! User IDs become numbers through `Names`. Those lookups touch memory far apart, so each row's lookups are started (address computed, cache line requested) and
//! finished a few rows later, when the memory has arrived.

use crate::ids::{hash_of, word, Key, Names, Supply, Table, LOW_BYTES};
use crate::os;
use crate::shard::{Deal, Dealer, Row};
use crate::scan::{rows_in, rows_wide, Index, Lens, Plain, Rows, SlowSqueeze, Squeeze, Wide};
use crate::stamp::{Dates, NEVER};
use crate::tables::{Campaigns, Products, Window};
use crate::ts::price_cents;
use crate::util::DAY_MS;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::RwLockReadGuard;

/// `who` of a click before its user number is known.
pub const NOBODY: u32 = u32::MAX;

/// One event row with an event_id, as dedup needs it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Sighting {
    /// ingest_ts; `i64::MAX` when unreadable.
    pub ingest: i64,
    /// Where the event was dealt; null when it is not valid.
    pub dealt: *mut Row,
    /// First byte of the row.
    pub row: *const u8,
    /// Hash of the event_id; its high half is the tag.
    pub hash: u64,
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

const _: () = assert!(std::mem::size_of::<Sighting>() == 32 && std::mem::size_of::<Ck>() == 24);

/// A stretch of records written one after another.
pub struct Run<T> {
    pub first: *mut T,
    pub n: usize,
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

/// One reader's event sightings, filed by cell range: a fixed bin per range in one region of
/// memory, and a list for whatever does not fit. The tags are kept apart too, side by side
/// with the sightings, so marking reads only them.
pub struct TagBins {
    room: usize,
    bins: *mut u32,
    seen: *mut Sighting,
    lens: Vec<usize>,
    spill: Vec<(Vec<u32>, Vec<Sighting>)>,
}

// SAFETY: the region is owned by this value alone, and shared only for reading.
unsafe impl Send for TagBins {}
unsafe impl Sync for TagBins {}

impl TagBins {
    /// Bins for about `rows` events.
    pub fn new(rows: usize) -> TagBins {
        let room = (rows / RANGES + rows / RANGES / 8 + 16).next_multiple_of(16);
        TagBins {
            room,
            bins: os::zeroed_array::<u32>(room * RANGES),
            seen: os::zeroed_array::<Sighting>(room * RANGES),
            lens: vec![0; RANGES],
            spill: (0..RANGES).map(|_| (Vec::new(), Vec::new())).collect(),
        }
    }
    #[inline(always)]
    pub fn push(&mut self, s: Sighting) {
        let tag = (s.hash >> 32) as u32;
        let r = range_of(tag);
        // SAFETY: `r` is below RANGES; the bin index is checked against its room.
        unsafe {
            let n = self.lens.get_unchecked_mut(r);
            if *n < self.room {
                let at = r * self.room + *n;
                *self.bins.add(at) = tag;
                self.seen.add(at).write(s);
                *n += 1;
            } else {
                let sp = self.spill.get_unchecked_mut(r);
                sp.0.push(tag);
                sp.1.push(s);
            }
        }
    }
    /// The tags and sightings of range `r`, in up to two pairs of slices.
    pub fn range(&self, r: usize) -> [(&[u32], &[Sighting]); 2] {
        // SAFETY: the first `lens[r]` items of bin `r` were written.
        let (tags, seen) = unsafe { (std::slice::from_raw_parts(self.bins.add(r * self.room), self.lens[r]), std::slice::from_raw_parts(self.seen.add(r * self.room), self.lens[r])) };
        [(tags, seen), (&self.spill[r].0[..], &self.spill[r].1[..])]
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
    /// Valid events, dealt into shards.
    pub dealer: Dealer,
    ck_free: *mut Ck,
    ck_left: usize,
    pub ck_runs: Vec<Run<Ck>>,
    /// Event sightings, filed by cell range (`range_of`).
    pub tags: TagBins,
}

impl Reader {
    /// A reader that expects to see about `events` event rows, dealing into `shards` shards.
    pub fn new(events: usize, shards: usize) -> Reader {
        Reader {
            ix: Index::new(),
            wx: Wide::new(),
            dates: Dates::new(),
            supply: Supply::new(),
            dealer: Dealer::new(shards),
            ck_free: std::ptr::null_mut(),
            ck_left: 0,
            ck_runs: Vec::new(),
            tags: TagBins::new(events),
        }
    }
}

/// A lookup that has been started, and for events what to do once it is done.
#[derive(Clone, Copy)]
struct Pending {
    /// Links and clicks: where the user number goes; null when there is none to find.
    into: *mut u32,
    key: Key,
    hash: u64,
    at: *const u8,
    len: usize,
    /// Events: the sighting (its `dealt` set once dealt), and what the dealt row needs.
    seen: Sighting,
    ev_key: u64,
    cat: u32,
    valid: bool,
}

const LAG: usize = 8;

/// Where an event reader's results go.
struct Sink<'a> {
    deal: &'a Deal<'a>,
    dealer: *mut Dealer,
    tags: *mut TagBins,
}

/// The lookups in flight, and the table they run against.
struct Flight<'a> {
    sh: &'a Shared<'a>,
    table: Table,
    guard: Option<RwLockReadGuard<'a, ()>>,
    ring: [Pending; LAG],
    pushed: usize,
    sink: Option<Sink<'a>>,
}

impl<'a> Flight<'a> {
    fn new(sh: &'a Shared<'a>, sink: Option<Sink<'a>>) -> Flight<'a> {
        let guard = sh.names.gate.read().unwrap();
        let table = sh.names.table();
        let mut f = Flight { sh, table, guard: Some(guard), ring: unsafe { std::mem::zeroed() }, pushed: 0, sink };
        f.ring = [f.idle(); LAG];
        f
    }

    #[inline(always)]
    unsafe fn finish(&self, supply: &mut Supply, p: &Pending) {
        if let Some(sink) = &self.sink {
            let mut seen = p.seen;
            if p.valid {
                let who = self.table.finish(self.sh.names, supply, &p.key, p.hash, p.at, p.len);
                seen.dealt = sink.deal.event(who, p.ev_key, p.cat, &mut *sink.dealer);
            }
            (*sink.tags).push(seen);
        } else if !p.into.is_null() {
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

    #[inline(always)]
    fn idle(&self) -> Pending {
        let seen = Sighting { ingest: 0, dealt: std::ptr::null_mut(), row: std::ptr::null(), hash: 0 };
        Pending { into: std::ptr::null_mut(), key: [0; 3], hash: 0, at: std::ptr::null(), len: 0, seen, ev_key: 0, cat: 0, valid: false }
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
}

impl Rows<6> for EventRows<'_, '_> {
    #[inline(always)]
    unsafe fn window(&mut self, _rows: usize) {
        self.fl.checkpoint(self.supply);
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
        let hash = hash_of(d.add(a0), e0 - a0);
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

        let ev_key = if ok { (((ts - sh.win.ws) as u64) << 24) | price as u64 } else { 0 };
        let seen = Sighting { ingest: if ingest == NEVER { i64::MAX } else { ingest }, dealt: std::ptr::null_mut(), row: d.add(from), hash };
        if SKIP & 8 != 0 {
            return;
        }
        let mut p = self.fl.idle();
        p.key = user_key;
        p.hash = user_hash;
        p.at = d.add(a1);
        p.len = user_len;
        p.seen = seen;
        p.ev_key = ev_key;
        p.cat = cat;
        p.valid = ok;
        self.fl.push(self.supply, p);
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
            self.runs.push(Run { first: self.run_first, n });
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
        let mut p = self.fl.idle();
        p.into = std::ptr::addr_of_mut!((*rec).who);
        p.key = user_key;
        p.hash = user_hash;
        p.at = d.add(a1);
        p.len = user_len;
        self.fl.push(self.supply, p);
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
            let mut p = self.fl.idle();
            p.into = slot.add(k);
            p.key = key;
            p.hash = hash;
            p.at = d.add(a);
            p.len = e - a;
            self.fl.push(self.supply, p);
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
unsafe fn read_with<L: Lens, Q: Squeeze>(sh: &Shared, deal: Option<&Deal>, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece, wide: bool) {
    if p.from >= p.to {
        return;
    }
    let data = p.data.as_ptr();
    match p.kind {
        Kind::Events => {
            let deal = deal.expect("events are read once the links are joined");
            let sink = Sink { deal, dealer: &mut r.dealer, tags: &mut r.tags };
            let mut rows = EventRows { fl: Flight::new(sh, Some(sink)), data, dates: &mut r.dates, supply: &mut r.supply };
            scan::<L, Q, _, 6>(wide, &mut r.ix, &mut r.wx, p, &mut rows);
            rows.fl.close(rows.supply);
        }
        Kind::Clicks => {
            let mut rows = ClickRows { fl: Flight::new(sh, None), data, dates: &mut r.dates, supply: &mut r.supply, runs: &mut r.ck_runs, free: r.ck_free, left: r.ck_left, run_first: r.ck_free };
            scan::<L, Q, _, 3>(wide, &mut r.ix, &mut r.wx, p, &mut rows);
            rows.fl.close(rows.supply);
            rows.end_run();
            let (free, left) = (rows.free, rows.left);
            r.ck_free = free;
            r.ck_left = left;
        }
        Kind::Links => {
            let mut rows = LinkRows { fl: Flight::new(sh, None), data, supply: &mut r.supply, pairs };
            scan::<L, Q, _, 1>(wide, &mut r.ix, &mut r.wx, p, &mut rows);
            rows.fl.close(rows.supply);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,popcnt")]
unsafe fn read_fast(sh: &Shared, deal: Option<&Deal>, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece) {
    read_with::<crate::scan::Avx, crate::scan::Bmi2>(sh, deal, r, pairs, p, true)
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
/// Events need `deal`.
pub fn read(sh: &Shared, deal: Option<&Deal>, r: &mut Reader, pairs: &mut Vec<[u32; 2]>, p: &Piece, fast: bool, scanner: Scanner) {
    // SAFETY: pieces lie inside mapped files, which carry a readable zero tail.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        if fast {
            return read_fast(sh, deal, r, pairs, p);
        }
        let _ = fast;
        read_with::<Plain, SlowSqueeze>(sh, deal, r, pairs, p, scanner == Scanner::Wide)
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
