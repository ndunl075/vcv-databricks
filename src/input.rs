//! Finding input files, making them readable as byte slices, and splitting them into
//! slices that start and end on row boundaries.

use crate::util::find_byte;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

mod os {
    use std::ffi::c_void;
    extern "C" {
        #[link_name = "mmap"]
        pub fn map_region(hint: *mut c_void, bytes: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
        #[link_name = "madvise"]
        pub fn advise_region(start: *mut c_void, bytes: usize, advice: i32) -> i32;
    }
    pub const READABLE: i32 = 1;
    pub const PRIVATE_COPY: i32 = 2;
    pub const READ_AHEAD: i32 = 2;
}

/// The whole content of a file, valid until the process ends. An empty file gives an empty slice.
pub fn open_view(path: &Path) -> std::io::Result<&'static [u8]> {
    let file = fs::File::open(path)?;
    let bytes = file.metadata()?.len() as usize;
    if bytes == 0 {
        return Ok(&[]);
    }
    // SAFETY: the mapping is private and read-only and is never unmapped.
    unsafe {
        let base = os::map_region(std::ptr::null_mut(), bytes, os::READABLE, os::PRIVATE_COPY, file.as_raw_fd(), 0);
        if base as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        os::advise_region(base, bytes, os::READ_AHEAD);
        Ok(std::slice::from_raw_parts(base as *const u8, bytes))
    }
}

/// Regular files named `*.csv` that sit directly in `dir`, in name order. No such directory: none.
pub fn list_csv(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().ends_with(".csv") {
            continue;
        }
        if fs::metadata(entry.path()).map(|m| m.is_file()).unwrap_or(false) {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Offset just past the first line (the header, whatever it holds).
pub fn first_row(data: &[u8]) -> usize {
    find_byte(data, 0, b'\n').map_or(data.len(), |i| i + 1)
}

/// Start of the first data row at or after `pos`; `data.len()` if there is none.
fn boundary(data: &[u8], pos: usize) -> usize {
    let first = first_row(data);
    if pos >= data.len() {
        return data.len();
    }
    let at = if pos == 0 { 0 } else { find_byte(data, pos - 1, b'\n').map_or(data.len(), |i| i + 1) };
    at.max(first)
}

/// A piece of one file, given by nominal byte offsets; `rows` snaps both ends to row starts, so
/// neighbouring pieces never share or split a row.
#[derive(Clone, Copy)]
pub struct Slice {
    pub file: usize,
    from: usize,
    to: usize,
}

impl Slice {
    pub fn rows(&self, data: &[u8]) -> (usize, usize) {
        let lo = boundary(data, self.from);
        let hi = boundary(data, self.to);
        (lo, hi.max(lo))
    }
}

/// Cuts every file into slices of about `piece` bytes.
pub fn plan_slices(files: &[&[u8]], piece: usize) -> Vec<Slice> {
    let mut plan = Vec::new();
    for (file, data) in files.iter().enumerate() {
        let count = data.len().div_ceil(piece).max(1);
        for k in 0..count {
            let to = if k + 1 == count { usize::MAX } else { (k + 1) * piece };
            plan.push(Slice { file, from: k * piece, to });
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leak(b: &[u8]) -> &'static [u8] {
        Box::leak(b.to_vec().into_boxed_slice())
    }

    #[test]
    fn slices_cover_each_row_once() {
        let data = leak(b"h1,h2\nl1\nline2\n\nlast");
        for piece in 1..=data.len() + 3 {
            let mut seen = Vec::new();
            for s in plan_slices(&[data], piece) {
                let (lo, hi) = s.rows(data);
                crate::csv::for_each_line(data, lo, hi, |l| seen.push(l.to_vec()));
            }
            let want: Vec<Vec<u8>> = vec![b"l1".to_vec(), b"line2".to_vec(), b"".to_vec(), b"last".to_vec()];
            assert_eq!(seen, want, "piece {piece}");
        }
    }

    #[test]
    fn no_data_rows() {
        for data in [&b"a,b\n"[..], &b"a,b"[..], &b""[..], &b"\n"[..]] {
            for s in plan_slices(&[data], 2) {
                let (lo, hi) = s.rows(data);
                let mut n = 0;
                crate::csv::for_each_line(data, lo, hi, |_| n += 1);
                assert_eq!(n, 0);
            }
        }
    }

    #[test]
    fn many_files_keep_their_index() {
        let (a, b) = (leak(b"h\nx\ny\n"), leak(b"h\nz\n"));
        let plan = plan_slices(&[a, b], 4);
        assert!(plan.iter().any(|s| s.file == 0) && plan.iter().any(|s| s.file == 1));
    }
}
