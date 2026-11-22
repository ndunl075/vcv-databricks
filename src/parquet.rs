//! Output tables as Parquet files: a single row group, plain-encoded data pages, nothing compressed.
//!
//! Page bodies are assembled column by column; the footer is described as a small tree of values
//! (`Tv`) that a generic routine turns into Thrift compact-protocol bytes.

use std::io::Write;
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

    /// Body of data page `p` (definition levels first when nullable).
    fn page_body(&self, p: usize, lo: usize, hi: usize) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            ColumnView::Int64(v) => v[lo..hi].iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ColumnView::Date(v) | ColumnView::Int32(v) => v[lo..hi].iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes())),
            ColumnView::Text(c) => out.extend_from_slice(slice_of_page(&c.bytes, &c.page_starts, p)),
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
                out.extend_from_slice(&(levels.len() as u32).to_le_bytes());
                out.extend_from_slice(&levels);
                out.extend_from_slice(slice_of_page(&c.bytes, &c.page_starts, p));
            }
        }
        out
    }
}

fn slice_of_page<'a>(bytes: &'a [u8], starts: &[usize], p: usize) -> &'a [u8] {
    let from = starts[p];
    let to = starts.get(p + 1).copied().unwrap_or(bytes.len());
    &bytes[from..to]
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

fn page_header(values: usize, body_len: usize) -> Vec<u8> {
    let header = vec![
        (1, Tv::I32(0)),
        (2, Tv::I32(body_len as i32)),
        (3, Tv::I32(body_len as i32)),
        (5, Tv::Struct(vec![(1, Tv::I32(values as i32)), (2, Tv::I32(0)), (3, Tv::I32(3)), (4, Tv::I32(3))])),
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

fn chunk_entry(name: &str, col: &ColumnView, rows: usize, offset: i64, size: i64) -> Vec<(i16, Tv)> {
    let meta = vec![
        (1, Tv::I32(col.physical())),
        (2, Tv::I32s(vec![0, 3])),
        (3, Tv::Texts(vec![name.to_string()])),
        (4, Tv::I32(0)),
        (5, Tv::I64(rows as i64)),
        (6, Tv::I64(size)),
        (7, Tv::I64(size)),
        (9, Tv::I64(offset)),
    ];
    vec![(2, Tv::I64(offset)), (3, Tv::Struct(meta))]
}

/// Writes `columns` (named by `names`) as one Parquet file at `path`.
pub fn write_table(path: &Path, names: &[&str], columns: &[ColumnView]) -> std::io::Result<()> {
    let rows = columns.first().map_or(0, |c| c.len());
    let mut file: Vec<u8> = b"PAR1".to_vec();
    let mut placed: Vec<(i64, i64)> = Vec::new();
    if rows > 0 {
        for col in columns {
            let begin = file.len();
            for (p, lo) in (0..rows).step_by(ROWS_PER_PAGE).enumerate() {
                let hi = (lo + ROWS_PER_PAGE).min(rows);
                let body = col.page_body(p, lo, hi);
                file.extend_from_slice(&page_header(hi - lo, body.len()));
                file.extend_from_slice(&body);
            }
            placed.push((begin as i64, (file.len() - begin) as i64));
        }
    }

    let mut schema = vec![vec![(4, Tv::Text("schema".to_string())), (5, Tv::I32(columns.len() as i32))]];
    schema.extend(columns.iter().zip(names).map(|(c, n)| schema_entry(n, c)));
    let groups = if rows > 0 {
        let chunks: Vec<_> = columns.iter().zip(names).zip(&placed).map(|((c, n), &(o, s))| chunk_entry(n, c, rows, o, s)).collect();
        let total: i64 = placed.iter().map(|p| p.1).sum();
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
    file.extend_from_slice(&footer);
    file.extend_from_slice(&(footer.len() as u32).to_le_bytes());
    file.extend_from_slice(b"PAR1");
    std::fs::File::create(path)?.write_all(&file)
}
