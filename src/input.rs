//! Input discovery, memory mapping and chunking.

use crate::util::find_byte;
use std::ffi::c_void;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::Path;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
    fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
}
const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MADV_SEQUENTIAL: i32 = 2;

/// Maps a whole file read-only for the life of the process. Empty file: empty slice.
pub fn map_file(path: &Path) -> std::io::Result<&'static [u8]> {
    let f = fs::File::open(path)?;
    let len = f.metadata()?.len() as usize;
    if len == 0 {
        return Ok(&[]);
    }
    unsafe {
        let p = mmap(std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, f.as_raw_fd(), 0);
        if p as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        madvise(p, len, MADV_SEQUENTIAL);
        Ok(std::slice::from_raw_parts(p as *const u8, len))
    }
}

/// Every `*.csv` file directly in `dir`, sorted by name. A missing directory is empty.
pub fn csv_files(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut v = Vec::new();
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(v),
        Err(e) => return Err(e),
    };
    for ent in rd {
        let ent = ent?;
        let name = ent.file_name();
        if !name.to_string_lossy().ends_with(".csv") {
            continue;
        }
        let md = match fs::metadata(ent.path()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_file() {
            v.push(ent.path());
        }
    }
    v.sort();
    Ok(v)
}

/// First line start at or after `pos`.
fn line_start_at(data: &[u8], pos: usize) -> usize {
    if pos == 0 {
        0
    } else if pos >= data.len() {
        data.len()
    } else {
        match find_byte(data, pos - 1, b'\n') {
            Some(i) => i + 1,
            None => data.len(),
        }
    }
}

/// Start of the first data line (the header is the first line, whatever it holds).
pub fn header_end(data: &[u8]) -> usize {
    match find_byte(data, 0, b'\n') {
        Some(i) => i + 1,
        None => data.len(),
    }
}

/// A nominal slice of a file; resolve it with `Task::range`.
#[derive(Clone, Copy)]
pub struct Task {
    pub file: usize,
    pub a: usize,
    pub b: usize,
    pub last: bool,
}

impl Task {
    /// Exact line-aligned byte range of this task, never overlapping its neighbours.
    pub fn range(&self, data: &[u8]) -> (usize, usize) {
        let h = header_end(data);
        let s = if self.a == 0 { h } else { line_start_at(data, self.a).max(h) };
        let e = if self.last { data.len() } else { line_start_at(data, self.b).max(h) };
        (s, e.max(s))
    }
}

pub fn make_tasks(files: &[&'static [u8]], chunk: usize) -> Vec<Task> {
    let mut v = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        let n = f.len().div_ceil(chunk).max(1);
        for k in 0..n {
            v.push(Task { file: fi, a: k * chunk, b: (k + 1) * chunk, last: k + 1 == n });
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_partition_lines() {
        let data: &'static [u8] = Box::leak(b"h1,h2\nl1\nline2\n\nlast".to_vec().into_boxed_slice());
        for chunk in 1..=data.len() + 2 {
            let tasks = make_tasks(&[data], chunk);
            let mut lines = Vec::new();
            for t in &tasks {
                let (s, e) = t.range(data);
                crate::csv::for_each_line(data, s, e, |l| lines.push(l.to_vec()));
            }
            let want: Vec<Vec<u8>> = vec![b"l1".to_vec(), b"line2".to_vec(), b"".to_vec(), b"last".to_vec()];
            assert_eq!(lines, want, "chunk {chunk}");
        }
    }

    #[test]
    fn header_only_and_empty() {
        for d in [&b"a,b\n"[..], &b"a,b"[..], &b""[..], &b"\n"[..]] {
            for t in make_tasks(&[d], 3) {
                let (s, e) = t.range(d);
                let mut n = 0;
                crate::csv::for_each_line(d, s, e, |_| n += 1);
                assert_eq!(n, 0);
            }
        }
    }
}
