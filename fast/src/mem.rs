//! Zero-filled anonymous regions: big arrays whose untouched pages cost nothing.

use std::ffi::c_void;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
    fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
}

#[cfg(target_os = "linux")]
const MAP_FLAGS: i32 = 0x02 | 0x20 | 0x4000; // PRIVATE | ANONYMOUS | NORESERVE
#[cfg(not(target_os = "linux"))]
const MAP_FLAGS: i32 = 0x02 | 0x1000; // PRIVATE | ANON

/// `n` zeroed elements. `T` must be valid when all-zero (atomics, integers).
/// Returns None if the system refuses the mapping.
pub fn zeroed<T>(n: usize, huge: bool) -> Option<&'static [T]> {
    let bytes = (n.max(1)).checked_mul(std::mem::size_of::<T>())?;
    let bytes = (bytes + 4095) & !4095;
    unsafe {
        let p = mmap(std::ptr::null_mut(), bytes, 3, MAP_FLAGS, -1, 0);
        if p as isize == -1 || p.is_null() {
            return None;
        }
        #[cfg(target_os = "linux")]
        if huge && bytes >= (4 << 20) {
            madvise(p, bytes, 14); // MADV_HUGEPAGE
        }
        let _ = (huge, madvise as usize);
        Some(std::slice::from_raw_parts(p as *const T, n))
    }
}
