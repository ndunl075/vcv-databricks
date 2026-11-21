//! Minimal Parquet writer: one row group, PLAIN encoding, no compression, data page v1.

use std::io::Write;
use std::path::Path;

pub const PAGE_ROWS: usize = 1 << 16;

/// Strings stored as PLAIN BYTE_ARRAY (u32 length + bytes), with a page mark every PAGE_ROWS rows.
#[derive(Default)]
pub struct StrCol {
    pub data: Vec<u8>,
    marks: Vec<usize>,
    pub n: usize,
}

impl StrCol {
    #[inline]
    pub fn push(&mut self, s: &[u8]) {
        if self.n % PAGE_ROWS == 0 {
            self.marks.push(self.data.len());
        }
        self.data.extend_from_slice(&(s.len() as u32).to_le_bytes());
        self.data.extend_from_slice(s);
        self.n += 1;
    }
}

/// Nullable string column: definition levels per row, values only for non-null rows.
#[derive(Default)]
pub struct OptStrCol {
    pub data: Vec<u8>,
    marks: Vec<usize>,
    pub def: Vec<u8>,
}

impl OptStrCol {
    #[inline]
    pub fn push(&mut self, s: Option<&[u8]>) {
        if self.def.len() % PAGE_ROWS == 0 {
            self.marks.push(self.data.len());
        }
        match s {
            Some(s) => {
                self.def.push(1);
                self.data.extend_from_slice(&(s.len() as u32).to_le_bytes());
                self.data.extend_from_slice(s);
            }
            None => self.def.push(0),
        }
    }
}

pub enum Col<'a> {
    I64(&'a [i64]),
    Date(&'a [i32]),
    Str(&'a StrCol),
    OptStr(&'a OptStrCol),
}

impl Col<'_> {
    fn rows(&self) -> usize {
        match self {
            Col::I64(v) => v.len(),
            Col::Date(v) => v.len(),
            Col::Str(c) => c.n,
            Col::OptStr(c) => c.def.len(),
        }
    }
}

// ---- Thrift compact protocol ----

struct Tw {
    b: Vec<u8>,
    last: Vec<i16>,
}

const T_I32: u8 = 5;
const T_I64: u8 = 6;
const T_BIN: u8 = 8;
const T_LIST: u8 = 9;
const T_STRUCT: u8 = 12;

impl Tw {
    fn new() -> Tw {
        Tw { b: Vec::new(), last: vec![0] }
    }
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.b.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.b.push(v as u8);
    }
    fn zz(&mut self, v: i64) {
        self.varint(((v << 1) ^ (v >> 63)) as u64);
    }
    fn field(&mut self, id: i16, ty: u8) {
        let l = *self.last.last().unwrap();
        let d = id - l;
        if d > 0 && d <= 15 {
            self.b.push(((d as u8) << 4) | ty);
        } else {
            self.b.push(ty);
            self.zz(id as i64);
        }
        *self.last.last_mut().unwrap() = id;
    }
    fn i32(&mut self, id: i16, v: i32) {
        self.field(id, T_I32);
        self.zz(v as i64);
    }
    fn i64(&mut self, id: i16, v: i64) {
        self.field(id, T_I64);
        self.zz(v);
    }
    fn string(&mut self, id: i16, s: &str) {
        self.field(id, T_BIN);
        self.varint(s.len() as u64);
        self.b.extend_from_slice(s.as_bytes());
    }
    /// Starts a struct-valued field.
    fn begin(&mut self, id: i16) {
        self.field(id, T_STRUCT);
        self.last.push(0);
    }
    /// Ends a struct (field-valued or list element).
    fn end(&mut self) {
        self.b.push(0);
        self.last.pop();
    }
    fn list(&mut self, id: i16, elem: u8, n: usize) {
        self.field(id, T_LIST);
        if n < 15 {
            self.b.push(((n as u8) << 4) | elem);
        } else {
            self.b.push(0xF0 | elem);
            self.varint(n as u64);
        }
    }
    /// Struct element inside a list.
    fn begin_elem(&mut self) {
        self.last.push(0);
    }
    fn str_elem(&mut self, s: &str) {
        self.varint(s.len() as u64);
        self.b.extend_from_slice(s.as_bytes());
    }
    fn i32_elem(&mut self, v: i32) {
        self.zz(v as i64);
    }
}

fn type_of(c: &Col) -> i32 {
    match c {
        Col::I64(_) => 2,
        Col::Date(_) => 1,
        _ => 6,
    }
}

fn page_header(num_values: usize, body: usize) -> Vec<u8> {
    let mut t = Tw::new();
    t.i32(1, 0);
    t.i32(2, body as i32);
    t.i32(3, body as i32);
    t.begin(5);
    t.i32(1, num_values as i32);
    t.i32(2, 0);
    t.i32(3, 3);
    t.i32(4, 3);
    t.end();
    t.b.push(0);
    t.b
}

fn page_slice<'a>(data: &'a [u8], marks: &[usize], p: usize) -> &'a [u8] {
    let a = marks[p];
    let b = if p + 1 < marks.len() { marks[p + 1] } else { data.len() };
    &data[a..b]
}

/// Writes one file. `names[i]` names column `cols[i]`.
pub fn write_file(path: &Path, names: &[&str], cols: &[Col]) -> std::io::Result<()> {
    let nrows = cols.first().map_or(0, |c| c.rows());
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"PAR1");
    let mut chunks: Vec<(i64, i64)> = Vec::new(); // (offset, size)
    if nrows > 0 {
        for c in cols {
            let start = out.len();
            let npages = nrows.div_ceil(PAGE_ROWS);
            for p in 0..npages {
                let r0 = p * PAGE_ROWS;
                let r1 = ((p + 1) * PAGE_ROWS).min(nrows);
                let nv = r1 - r0;
                let mut body: Vec<u8> = Vec::new();
                match c {
                    Col::I64(v) => {
                        body.reserve(nv * 8);
                        for x in &v[r0..r1] {
                            body.extend_from_slice(&x.to_le_bytes());
                        }
                    }
                    Col::Date(v) => {
                        body.reserve(nv * 4);
                        for x in &v[r0..r1] {
                            body.extend_from_slice(&x.to_le_bytes());
                        }
                    }
                    Col::Str(s) => body.extend_from_slice(page_slice(&s.data, &s.marks, p)),
                    Col::OptStr(s) => {
                        let groups = nv.div_ceil(8);
                        let mut lv = vec![0u8; groups];
                        for (i, &d) in s.def[r0..r1].iter().enumerate() {
                            lv[i >> 3] |= d << (i & 7);
                        }
                        let mut t = Tw::new();
                        t.varint(((groups as u64) << 1) | 1);
                        t.b.extend_from_slice(&lv);
                        body.extend_from_slice(&(t.b.len() as u32).to_le_bytes());
                        body.extend_from_slice(&t.b);
                        body.extend_from_slice(page_slice(&s.data, &s.marks, p));
                    }
                }
                out.extend_from_slice(&page_header(nv, body.len()));
                out.extend_from_slice(&body);
            }
            chunks.push((start as i64, (out.len() - start) as i64));
        }
    }
    // Footer: FileMetaData.
    let mut t = Tw::new();
    t.i32(1, 1);
    t.list(2, T_STRUCT, cols.len() + 1);
    t.begin_elem();
    t.string(4, "schema");
    t.i32(5, cols.len() as i32);
    t.end();
    for (i, c) in cols.iter().enumerate() {
        t.begin_elem();
        t.i32(1, type_of(c));
        t.i32(3, if matches!(c, Col::OptStr(_)) { 1 } else { 0 });
        t.string(4, names[i]);
        match c {
            Col::Str(_) | Col::OptStr(_) => {
                t.i32(6, 0);
                t.begin(10);
                t.begin(1);
                t.end();
                t.end();
            }
            Col::Date(_) => {
                t.i32(6, 6);
                t.begin(10);
                t.begin(6);
                t.end();
                t.end();
            }
            _ => {}
        }
        t.end();
    }
    t.i64(3, nrows as i64);
    if nrows > 0 {
        t.list(4, T_STRUCT, 1);
        t.begin_elem();
        t.list(1, T_STRUCT, cols.len());
        for (i, c) in cols.iter().enumerate() {
            let (off, size) = chunks[i];
            t.begin_elem();
            t.i64(2, off);
            t.begin(3);
            t.i32(1, type_of(c));
            t.list(2, T_I32, 2);
            t.i32_elem(0);
            t.i32_elem(3);
            t.list(3, T_BIN, 1);
            t.str_elem(names[i]);
            t.i32(4, 0);
            t.i64(5, nrows as i64);
            t.i64(6, size);
            t.i64(7, size);
            t.i64(9, off);
            t.end();
            t.end();
        }
        let total: i64 = chunks.iter().map(|c| c.1).sum();
        t.i64(2, total);
        t.i64(3, nrows as i64);
        t.end();
    } else {
        t.list(4, T_STRUCT, 0);
    }
    t.string(6, "shopping-sessions");
    t.b.push(0);
    let meta = t.b;
    out.extend_from_slice(&meta);
    out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    out.extend_from_slice(b"PAR1");
    let mut f = std::fs::File::create(path)?;
    f.write_all(&out)?;
    Ok(())
}
