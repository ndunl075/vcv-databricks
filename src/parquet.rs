//! Output tables as Parquet files: a single row group, nothing compressed. A column whose values
//! repeat is dictionary-encoded (one PLAIN dictionary page, then RLE_DICTIONARY data pages whose
//! indices use the RLE / bit-packed hybrid), which is what pyarrow writes by default; other columns
//! use plain data pages. Smaller files matter: the bytes written are flushed to disk when the
//! container is torn down, and that time counts.
//!
//! Page bodies are assembled column by column; the footer is described as a small tree of values
//! (`Tv`) that a generic routine turns into Thrift compact-protocol bytes.

use std::io::{IoSlice, Write};
use std::path::Path;

/// Rows per data page.
pub const ROWS_PER_PAGE: usize = 1 << 16;

/// Text values, each stored as a 4-byte little-endian length plus the bytes.
#[derive(Default)]
pub struct TextColumn {
    bytes: Vec<u8>,
    page_starts: Vec<usize>,
    rows: usize,
}

impl TextColumn {
    /// Room for `rows` values of about `bytes_each` bytes.
    pub fn with_room(rows: usize, bytes_each: usize) -> TextColumn {
        TextColumn { bytes: Vec::with_capacity(rows * bytes_each), page_starts: Vec::new(), rows: 0 }
    }
    #[inline]
    pub fn push(&mut self, v: &[u8]) {
        if self.rows % ROWS_PER_PAGE == 0 {
            self.page_starts.push(self.bytes.len());
        }
        self.bytes.extend_from_slice(&(v.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(v);
        self.rows += 1;
    }
}

/// Text values that may be null: a presence flag per row, bytes only for present values.
#[derive(Default)]
pub struct MaybeTextColumn {
    bytes: Vec<u8>,
    page_starts: Vec<usize>,
    present: Vec<u8>,
}

impl MaybeTextColumn {
    /// Room for `rows` rows whose values take about `bytes_each` bytes on average.
    pub fn with_room(rows: usize, bytes_each: usize) -> MaybeTextColumn {
        MaybeTextColumn { bytes: Vec::with_capacity(rows * bytes_each), page_starts: Vec::new(), present: Vec::with_capacity(rows) }
    }
    #[inline]
    pub fn push(&mut self, v: Option<&[u8]>) {
        if self.present.len() % ROWS_PER_PAGE == 0 {
            self.page_starts.push(self.bytes.len());
        }
        self.present.push(v.is_some() as u8);
        if let Some(v) = v {
            self.bytes.extend_from_slice(&(v.len() as u32).to_le_bytes());
            self.bytes.extend_from_slice(v);
        }
    }
}

/// One column of a table, borrowed from the builder that owns the values.
pub enum ColumnView<'a> {
    Int64(&'a [i64]),
    Int32(&'a [i32]),
    Date(&'a [i32]),
    Text(&'a TextColumn),
    MaybeText(&'a MaybeTextColumn),
}

impl ColumnView<'_> {
    fn len(&self) -> usize {
        match self {
            ColumnView::Int64(v) => v.len(),
            ColumnView::Int32(v) => v.len(),
            ColumnView::Date(v) => v.len(),
            ColumnView::Text(c) => c.rows,
            ColumnView::MaybeText(c) => c.present.len(),
        }
    }

    /// Parquet physical type number.
    fn physical(&self) -> i32 {
        match self {
            ColumnView::Int64(_) => 2,
            ColumnView::Date(_) | ColumnView::Int32(_) => 1,
            _ => 6,
        }
    }

    fn nullable(&self) -> bool {
        matches!(self, ColumnView::MaybeText(_))
    }

    /// Body of data page `p` as one or two pieces: a small owned prefix (the definition
    /// levels of a nullable column) and the values, borrowed from the column.
    fn page_body(&self, p: usize, lo: usize, hi: usize) -> (Vec<u8>, &[u8]) {
        match self {
            ColumnView::Int64(v) => (Vec::new(), raw_bytes(&v[lo..hi])),
            ColumnView::Date(v) | ColumnView::Int32(v) => (Vec::new(), raw_bytes(&v[lo..hi])),
            ColumnView::Text(c) => (Vec::new(), slice_of_page(&c.bytes, &c.page_starts, p)),
            ColumnView::MaybeText(c) => {
                // Definition levels: one bit-packed run of 1-bit values.
                let groups = (hi - lo).div_ceil(8);
                let mut packed = vec![0u8; groups];
                for (k, &flag) in c.present[lo..hi].iter().enumerate() {
                    packed[k / 8] |= flag << (k % 8);
                }
                let mut levels = Vec::new();
                put_varint(&mut levels, ((groups as u64) << 1) | 1);
                levels.extend_from_slice(&packed);
                let mut out = (levels.len() as u32).to_le_bytes().to_vec();
                out.extend_from_slice(&levels);
                (out, slice_of_page(&c.bytes, &c.page_starts, p))
            }
        }
    }
}

#[cfg(not(target_endian = "little"))]
compile_error!("the Parquet writer stores integers as they sit in memory");

/// The values of an integer column as the bytes Parquet's plain encoding wants.
fn raw_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: integers have no padding; on a little-endian machine their bytes are the encoding.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn slice_of_page<'a>(bytes: &'a [u8], starts: &[usize], p: usize) -> &'a [u8] {
    let from = starts[p];
    let to = starts.get(p + 1).copied().unwrap_or(bytes.len());
    &bytes[from..to]
}

// ---------------------------------------------------------------------------------------------
// Dictionary encoding.

/// A dictionary-encoded column chunk: the dictionary page body (PLAIN values), how many values it
/// holds, the bit width of an index, and one index per non-null value, in row order.
struct Dict {
    values: Vec<u8>,
    n: usize,
    width: u32,
    idx: Vec<u32>,
}

/// Cheap multiplicative hasher for the dictionary tables.
#[derive(Default)]
struct Mix(u64);

impl std::hash::Hasher for Mix {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0 ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            h = (h ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0xff51_afd7_ed55_8ccd).rotate_left(29);
        }
        let mut last = [0u8; 8];
        last[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
        h = (h ^ u64::from_le_bytes(last)).wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        self.0 = h ^ (h >> 31);
    }
    fn write_u64(&mut self, v: u64) {
        let h = (self.0 ^ v).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = h ^ (h >> 29);
    }
}

type MixMap<K> = std::collections::HashMap<K, u32, std::hash::BuildHasherDefault<Mix>>;

fn width_for(n: usize) -> u32 {
    (usize::BITS - n.saturating_sub(1).leading_zeros()).max(1)
}

/// Most distinct values a chunk of `rows` values may have and still be dictionary-encoded.
fn dict_limit(rows: usize) -> usize {
    (rows / 2).max(64)
}

/// Text values (4-byte length + bytes each, back to back) as a dictionary, or None when they
/// barely repeat.
fn dict_texts(bytes: &[u8], count: usize) -> Option<Dict> {
    let limit = dict_limit(count);
    let mut map: MixMap<&[u8]> = MixMap::default();
    let mut values = Vec::new();
    let mut idx = Vec::with_capacity(count);
    let mut prev: (&[u8], u32) = (&[], u32::MAX);
    let mut at = 0usize;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let v = &bytes[at + 4..at + 4 + len];
        at += 4 + len;
        if prev.1 != u32::MAX && prev.0 == v {
            idx.push(prev.1);
            continue;
        }
        let next = map.len() as u32;
        let i = *map.entry(v).or_insert(next);
        if i == next {
            if map.len() > limit {
                return None;
            }
            values.extend_from_slice(&(len as u32).to_le_bytes());
            values.extend_from_slice(v);
        }
        idx.push(i);
        prev = (v, i);
    }
    let n = map.len();
    Some(Dict { values, n, width: width_for(n), idx })
}

/// Text values as a dictionary with one entry per run of equal neighbours, without hashing. The
/// user table holds each user's rows together and each user once, so its runs are its distinct
/// values; any other column only risks a repeated dictionary entry, which readers accept.
fn dict_text_runs(bytes: &[u8], count: usize) -> Option<Dict> {
    let limit = dict_limit(count);
    let mut values = Vec::with_capacity(bytes.len() / 2);
    let mut idx = Vec::with_capacity(count);
    let mut prev: &[u8] = &[];
    let mut n = 0u32;
    let mut at = 0usize;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let v = &bytes[at + 4..at + 4 + len];
        if n == 0 || v != prev {
            if n as usize >= limit {
                return None;
            }
            values.extend_from_slice(&bytes[at..at + 4 + len]);
            prev = v;
            n += 1;
        }
        idx.push(n - 1);
        at += 4 + len;
    }
    let n = n as usize;
    Some(Dict { values, n, width: width_for(n), idx })
}

/// Integer values as a dictionary of `size`-byte plain values, or None when they barely repeat.
fn dict_ints(v: impl Iterator<Item = i64> + Clone, count: usize, size: usize) -> Option<Dict> {
    let limit = dict_limit(count);
    let (lo, hi) = v.clone().fold((i64::MAX, i64::MIN), |(a, b), x| (a.min(x), b.max(x)));
    if count > 0 && (hi as i128 - lo as i128) < (1 << 16) {
        // Small range: a direct table from value to index.
        let mut slot = vec![u32::MAX; (hi - lo) as usize + 1];
        let mut values = Vec::new();
        let mut idx = Vec::with_capacity(count);
        let mut n = 0u32;
        for x in v {
            let s = &mut slot[(x - lo) as usize];
            if *s == u32::MAX {
                *s = n;
                n += 1;
                values.extend_from_slice(&x.to_le_bytes()[..size]);
            }
            idx.push(*s);
        }
        if n as usize > limit {
            return None;
        }
        let n = n as usize;
        return Some(Dict { values, n, width: width_for(n), idx });
    }
    let mut map: MixMap<i64> = MixMap::default();
    let mut values = Vec::new();
    let mut idx = Vec::with_capacity(count);
    let mut prev = (0i64, u32::MAX);
    for x in v {
        if prev.1 != u32::MAX && prev.0 == x {
            idx.push(prev.1);
            continue;
        }
        let next = map.len() as u32;
        let i = *map.entry(x).or_insert(next);
        if i == next {
            if map.len() > limit {
                return None;
            }
            values.extend_from_slice(&x.to_le_bytes()[..size]);
        }
        idx.push(i);
        prev = (x, i);
    }
    let n = map.len();
    Some(Dict { values, n, width: width_for(n), idx })
}

/// Appends `v` in Parquet's RLE / bit-packed hybrid encoding (no length prefix). Runs of at
/// least eight equal values become RLE runs; the rest is bit-packed eight values at a time, the
/// last group padded with zeros.
fn put_hybrid(out: &mut Vec<u8>, v: &[u32], width: u32) {
    let value_bytes = width.div_ceil(8) as usize;
    let n = v.len();
    let run_at = |i: usize, cap: usize| -> usize {
        let mut r = 1;
        while r < cap && i + r < n && v[i + r] == v[i] {
            r += 1;
        }
        r
    };
    let mut i = 0;
    while i < n {
        let r = run_at(i, usize::MAX);
        if r >= 8 {
            put_varint(out, (r as u64) << 1);
            out.extend_from_slice(&v[i].to_le_bytes()[..value_bytes]);
            i += r;
            continue;
        }
        let start = i;
        let mut groups = 0usize;
        loop {
            i += 8;
            groups += 1;
            if i >= n || run_at(i, 8) >= 8 {
                break;
            }
        }
        put_varint(out, ((groups as u64) << 1) | 1);
        let mut acc = 0u64;
        let mut bits = 0u32;
        for k in start..start + groups * 8 {
            let x = if k < n { v[k] as u64 } else { 0 };
            acc |= x << bits;
            bits += width;
            while bits >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        i = i.min(n);
    }
}

impl ColumnView<'_> {
    /// The dictionary encoding of this column, when it pays.
    fn dictionary(&self) -> Option<Dict> {
        let rows = self.len();
        let d = match self {
            ColumnView::Int64(v) => dict_ints(v.iter().map(|&x| x), rows, 8)?,
            ColumnView::Int32(v) | ColumnView::Date(v) => dict_ints(v.iter().map(|&x| x as i64), rows, 4)?,
            ColumnView::Text(c) => dict_text_runs(&c.bytes, c.rows)?,
            ColumnView::MaybeText(c) => dict_texts(&c.bytes, c.present.iter().filter(|&&f| f != 0).count())?,
        };
        let plain = match self {
            ColumnView::Int64(v) => v.len() * 8,
            ColumnView::Int32(v) | ColumnView::Date(v) => v.len() * 4,
            ColumnView::Text(c) => c.bytes.len(),
            ColumnView::MaybeText(c) => c.bytes.len(),
        };
        // Upper bound of the encoded size (no RLE runs counted).
        let coded = d.values.len() + d.idx.len() * d.width as usize / 8 + d.idx.len() / 8 + 16;
        (coded < plain).then_some(d)
    }

    /// Definition levels of rows `lo..hi` of a nullable column, with their 4-byte length, and how
    /// many of those rows hold a value.
    fn levels(&self, lo: usize, hi: usize) -> (Vec<u8>, usize) {
        match self {
            ColumnView::MaybeText(c) => {
                let flags = &c.present[lo..hi];
                let groups = flags.len().div_ceil(8);
                let mut packed = vec![0u8; groups];
                for (k, &flag) in flags.iter().enumerate() {
                    packed[k / 8] |= flag << (k % 8);
                }
                let mut levels = Vec::new();
                put_varint(&mut levels, ((groups as u64) << 1) | 1);
                levels.extend_from_slice(&packed);
                let mut out = (levels.len() as u32).to_le_bytes().to_vec();
                out.extend_from_slice(&levels);
                (out, flags.iter().filter(|&&f| f != 0).count())
            }
            _ => (Vec::new(), hi - lo),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Thrift compact protocol, driven by a value tree.

/// A Thrift value.
enum Tv {
    I32(i32),
    I64(i64),
    Text(String),
    Struct(Vec<(i16, Tv)>),
    I32s(Vec<i32>),
    Texts(Vec<String>),
    Structs(Vec<Vec<(i16, Tv)>>),
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

fn put_zigzag(out: &mut Vec<u8>, v: i64) {
    put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
}

fn wire_type(v: &Tv) -> u8 {
    match v {
        Tv::I32(_) => 5,
        Tv::I64(_) => 6,
        Tv::Text(_) => 8,
        Tv::I32s(_) | Tv::Texts(_) | Tv::Structs(_) => 9,
        Tv::Struct(_) => 12,
    }
}

fn put_list_head(out: &mut Vec<u8>, count: usize, elem: u8) {
    if count < 15 {
        out.push(((count as u8) << 4) | elem);
    } else {
        out.push(0xf0 | elem);
        put_varint(out, count as u64);
    }
}

fn put_struct(out: &mut Vec<u8>, fields: &[(i16, Tv)]) {
    let mut prev = 0i16;
    for (id, v) in fields {
        let delta = id - prev;
        let t = wire_type(v);
        if (1..=15).contains(&delta) {
            out.push(((delta as u8) << 4) | t);
        } else {
            out.push(t);
            put_zigzag(out, *id as i64);
        }
        prev = *id;
        match v {
            Tv::I32(x) => put_zigzag(out, *x as i64),
            Tv::I64(x) => put_zigzag(out, *x),
            Tv::Text(s) => {
                put_varint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            Tv::Struct(inner) => put_struct(out, inner),
            Tv::I32s(xs) => {
                put_list_head(out, xs.len(), 5);
                xs.iter().for_each(|x| put_zigzag(out, *x as i64));
            }
            Tv::Texts(ss) => {
                put_list_head(out, ss.len(), 8);
                for s in ss {
                    put_varint(out, s.len() as u64);
                    out.extend_from_slice(s.as_bytes());
                }
            }
            Tv::Structs(items) => {
                put_list_head(out, items.len(), 12);
                items.iter().for_each(|f| put_struct(out, f));
            }
        }
    }
    out.push(0);
}

/// Encodings: plain, RLE (levels), RLE_DICTIONARY.
const PLAIN: i32 = 0;
const RLE: i32 = 3;
const RLE_DICTIONARY: i32 = 8;

fn page_header(values: usize, body_len: usize, encoding: i32) -> Vec<u8> {
    let header = vec![
        (1, Tv::I32(0)),
        (2, Tv::I32(body_len as i32)),
        (3, Tv::I32(body_len as i32)),
        (5, Tv::Struct(vec![(1, Tv::I32(values as i32)), (2, Tv::I32(encoding)), (3, Tv::I32(RLE)), (4, Tv::I32(RLE))])),
    ];
    let mut out = Vec::new();
    put_struct(&mut out, &header);
    out
}

fn dictionary_page_header(values: usize, body_len: usize) -> Vec<u8> {
    let header = vec![
        (1, Tv::I32(2)),
        (2, Tv::I32(body_len as i32)),
        (3, Tv::I32(body_len as i32)),
        (7, Tv::Struct(vec![(1, Tv::I32(values as i32)), (2, Tv::I32(PLAIN))])),
    ];
    let mut out = Vec::new();
    put_struct(&mut out, &header);
    out
}

fn schema_entry(name: &str, col: &ColumnView) -> Vec<(i16, Tv)> {
    let mut e = vec![(1, Tv::I32(col.physical())), (3, Tv::I32(col.nullable() as i32)), (4, Tv::Text(name.to_string()))];
    match col {
        ColumnView::Text(_) | ColumnView::MaybeText(_) => {
            e.push((6, Tv::I32(0)));
            e.push((10, Tv::Struct(vec![(1, Tv::Struct(vec![]))])));
        }
        ColumnView::Date(_) => {
            e.push((6, Tv::I32(6)));
            e.push((10, Tv::Struct(vec![(6, Tv::Struct(vec![]))])));
        }
        ColumnView::Int64(_) | ColumnView::Int32(_) => {}
    }
    e
}

/// Where a column chunk sits: its first byte, its size, and its first data page when a
/// dictionary page comes first.
struct Placed {
    begin: i64,
    size: i64,
    data_page: Option<i64>,
}

fn chunk_entry(name: &str, col: &ColumnView, rows: usize, at: &Placed) -> Vec<(i16, Tv)> {
    let encodings = if at.data_page.is_some() { vec![PLAIN, RLE, RLE_DICTIONARY] } else { vec![PLAIN, RLE] };
    let mut meta = vec![
        (1, Tv::I32(col.physical())),
        (2, Tv::I32s(encodings)),
        (3, Tv::Texts(vec![name.to_string()])),
        (4, Tv::I32(0)),
        (5, Tv::I64(rows as i64)),
        (6, Tv::I64(at.size)),
        (7, Tv::I64(at.size)),
        (9, Tv::I64(at.data_page.unwrap_or(at.begin))),
    ];
    if at.data_page.is_some() {
        meta.push((11, Tv::I64(at.begin)));
    }
    vec![(2, Tv::I64(at.begin)), (3, Tv::Struct(meta))]
}

/// A piece of the file: bytes made for it, or bytes that already sit in a column.
enum Piece<'a> {
    Made(Vec<u8>),
    Lent(&'a [u8]),
}

/// Writes `columns` (named by `names`) as one Parquet file at `path`. Column values go to the
/// file straight from where they are.
pub fn write_table(path: &Path, names: &[&str], columns: &[ColumnView]) -> std::io::Result<()> {
    let rows = columns.first().map_or(0, |c| c.len());
    let mut pieces: Vec<Piece> = vec![Piece::Made(b"PAR1".to_vec())];
    let mut at = 4usize;
    let mut placed: Vec<Placed> = Vec::new();
    if rows > 0 {
        for col in columns {
            let begin = at;
            let dict = col.dictionary();
            let mut data_page = None;
            if let Some(d) = &dict {
                let mut head = dictionary_page_header(d.n, d.values.len());
                head.extend_from_slice(&d.values);
                at += head.len();
                pieces.push(Piece::Made(head));
                data_page = Some(at as i64);
            }
            let mut done = 0usize;
            for (p, lo) in (0..rows).step_by(ROWS_PER_PAGE).enumerate() {
                let hi = (lo + ROWS_PER_PAGE).min(rows);
                match &dict {
                    None => {
                        let (prefix, values) = col.page_body(p, lo, hi);
                        let mut head = page_header(hi - lo, prefix.len() + values.len(), PLAIN);
                        head.extend_from_slice(&prefix);
                        at += head.len() + values.len();
                        pieces.push(Piece::Made(head));
                        pieces.push(Piece::Lent(values));
                    }
                    Some(d) => {
                        let (mut body, present) = col.levels(lo, hi);
                        body.push(d.width as u8);
                        put_hybrid(&mut body, &d.idx[done..done + present], d.width);
                        done += present;
                        let mut head = page_header(hi - lo, body.len(), RLE_DICTIONARY);
                        head.extend_from_slice(&body);
                        at += head.len();
                        pieces.push(Piece::Made(head));
                    }
                }
            }
            placed.push(Placed { begin: begin as i64, size: (at - begin) as i64, data_page });
        }
    }

    let mut schema = vec![vec![(4, Tv::Text("schema".to_string())), (5, Tv::I32(columns.len() as i32))]];
    schema.extend(columns.iter().zip(names).map(|(c, n)| schema_entry(n, c)));
    let groups = if rows > 0 {
        let chunks: Vec<_> = columns.iter().zip(names).zip(&placed).map(|((c, n), at)| chunk_entry(n, c, rows, at)).collect();
        let total: i64 = placed.iter().map(|p| p.size).sum();
        vec![vec![(1, Tv::Structs(chunks)), (2, Tv::I64(total)), (3, Tv::I64(rows as i64))]]
    } else {
        vec![]
    };
    let footer_tree = vec![
        (1, Tv::I32(1)),
        (2, Tv::Structs(schema)),
        (3, Tv::I64(rows as i64)),
        (4, Tv::Structs(groups)),
        (6, Tv::Text("shopping-sessions".to_string())),
    ];
    let mut footer = Vec::new();
    put_struct(&mut footer, &footer_tree);
    let footer_len = footer.len() as u32;
    footer.extend_from_slice(&footer_len.to_le_bytes());
    footer.extend_from_slice(b"PAR1");
    pieces.push(Piece::Made(footer));

    let mut slices: Vec<IoSlice> = pieces
        .iter()
        .map(|p| match p {
            Piece::Made(v) => IoSlice::new(v),
            Piece::Lent(v) => IoSlice::new(v),
        })
        .filter(|s| !s.is_empty())
        .collect();
    let mut file = std::fs::File::create(path)?;
    let mut rest = &mut slices[..];
    while !rest.is_empty() {
        // Few pieces per call: some systems cap how many one write may carry.
        let take = rest.len().min(64);
        let n = file.write_vectored(&rest[..take])?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "could not write the table"));
        }
        IoSlice::advance_slices(&mut rest, n);
    }
    Ok(())
}

#[cfg(test)]
mod dictionary_tests {
    use super::*;

    /// Reads back `n` values of the RLE / bit-packed hybrid.
    fn read_hybrid(mut b: &[u8], width: u32, n: usize) -> Vec<u32> {
        let varint = |b: &mut &[u8]| {
            let (mut v, mut s) = (0u64, 0);
            loop {
                let x = b[0];
                *b = &b[1..];
                v |= ((x & 0x7f) as u64) << s;
                s += 7;
                if x < 0x80 {
                    return v;
                }
            }
        };
        let mut out = Vec::new();
        while out.len() < n {
            let h = varint(&mut b);
            if h & 1 == 0 {
                let k = width.div_ceil(8) as usize;
                let mut v = [0u8; 4];
                v[..k].copy_from_slice(&b[..k]);
                b = &b[k..];
                out.extend(std::iter::repeat(u32::from_le_bytes(v)).take((h >> 1) as usize));
            } else {
                let count = (h >> 1) as usize * 8;
                let (mut acc, mut bits, mut used) = (0u64, 0u32, 0usize);
                for _ in 0..count {
                    while bits < width {
                        acc |= (b[used] as u64) << bits;
                        used += 1;
                        bits += 8;
                    }
                    out.push((acc & ((1u64 << width) - 1)) as u32);
                    acc >>= width;
                    bits -= width;
                }
                b = &b[used..];
            }
        }
        out.truncate(n);
        out
    }

    #[test]
    fn hybrid_round_trips() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut rnd = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for case in 0..400 {
            let width = 1 + (case % 24) as u32;
            let n = rnd(3000) as usize;
            let mut v = Vec::with_capacity(n);
            while v.len() < n {
                let x = (rnd(1 << width)) as u32;
                let r = if rnd(3) == 0 { 1 + rnd(40) as usize } else { 1 };
                v.extend(std::iter::repeat(x).take(r.min(n - v.len())));
            }
            let mut b = Vec::new();
            put_hybrid(&mut b, &v, width);
            assert_eq!(read_hybrid(&b, width, n), v, "width {width} n {n}");
        }
    }

    #[test]
    fn dictionaries_keep_first_seen_order() {
        let d = dict_ints([5i64, 5, 7, 5, 9, 7].into_iter(), 6, 8).unwrap();
        assert_eq!(d.idx, vec![0, 0, 1, 0, 2, 1]);
        assert_eq!(d.n, 3);
        let mut c = TextColumn::default();
        for s in ["b", "b", "a", "", "b", "a"] {
            c.push(s.as_bytes());
        }
        let d = dict_texts(&c.bytes, c.rows).unwrap();
        assert_eq!(d.idx, vec![0, 0, 1, 2, 0, 1]);
        assert_eq!(d.values, b"\x01\0\0\0b\x01\0\0\0a\0\0\0\0".to_vec());
    }
}
