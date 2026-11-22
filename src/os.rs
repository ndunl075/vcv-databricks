//! The handful of system calls std does not offer: memory regions, paging advice, a hard exit,
//! and cache-line prefetch hints.

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::AsRawFd;

mod sys {
    use std::ffi::c_void;
    extern "C" {
        #[link_name = "mmap"]
        pub fn region(hint: *mut c_void, bytes: usize, access: i32, kind: i32, fd: i32, at: i64) -> *mut c_void;
        #[link_name = "madvise"]
        pub fn advise(start: *mut c_void, bytes: usize, what: i32) -> i32;
        #[link_name = "_exit"]
        pub fn leave(code: i32) -> !;
        #[link_name = "getrusage"]
        pub fn usage(who: i32, out: *mut [i64; 18]) -> i32;
    }
    pub const CAN_READ: i32 = 1;
    pub const CAN_WRITE: i32 = 2;
    pub const OWN_COPY: i32 = 2;
    pub const IN_PLACE: i32 = 0x10;
    #[cfg(target_os = "linux")]
    pub const NO_FILE: i32 = 0x20;
    #[cfg(not(target_os = "linux"))]
    pub const NO_FILE: i32 = 0x1000;
    pub const DROP_PAGES: i32 = 4;
    #[cfg(target_os = "linux")]
    pub const BIG_PAGES: i32 = 14;
}

/// Bytes of zero padding guaranteed readable after every mapped file.
pub const TAIL: usize = 1 << 16;

fn fail(what: &str) -> ! {
    eprintln!("error: {what}: {}", std::io::Error::last_os_error());
    std::process::exit(1)
}

/// Regions at least this large start on a huge-page boundary.
const BIG: usize = 2 << 20;

/// `bytes` of zeroed, writable memory that lives until the process ends. Large regions start on
/// a 2 MB boundary and ask for huge pages where the system offers them on request.
pub fn zeroed(bytes: usize) -> *mut u8 {
    let bytes = bytes.max(1).next_multiple_of(TAIL);
    let big = bytes >= BIG;
    let span = if big { bytes.next_multiple_of(BIG) + BIG } else { bytes };
    // SAFETY: a fresh private anonymous mapping.
    unsafe {
        let p = sys::region(std::ptr::null_mut(), span, sys::CAN_READ | sys::CAN_WRITE, sys::OWN_COPY | sys::NO_FILE, -1, 0);
        if p as isize == -1 {
            fail("memory region");
        }
        if !big {
            return p as *mut u8;
        }
        let start = (p as usize).next_multiple_of(BIG) as *mut c_void;
        #[cfg(target_os = "linux")]
        sys::advise(start, span - BIG, sys::BIG_PAGES);
        start as *mut u8
    }
}

/// Typed, zeroed array of `n` items (see `zeroed`). `T` must be valid when all its bytes are zero.
pub fn zeroed_array<T>(n: usize) -> *mut T {
    zeroed(n * std::mem::size_of::<T>()) as *mut T
}

/// The whole file as bytes, followed by at least `TAIL` readable zero bytes. Never unmapped.
pub fn file_bytes(file: &File, bytes: usize) -> &'static [u8] {
    if bytes == 0 {
        return &[];
    }
    let span = bytes.next_multiple_of(TAIL) + TAIL;
    // SAFETY: reserve a read-only zero region, then lay the file over its start.
    unsafe {
        let base = sys::region(std::ptr::null_mut(), span, sys::CAN_READ, sys::OWN_COPY | sys::NO_FILE, -1, 0);
        if base as isize == -1 {
            fail("address space for input");
        }
        let got = sys::region(base, bytes, sys::CAN_READ, sys::OWN_COPY | sys::IN_PLACE, file.as_raw_fd(), 0);
        if got as isize == -1 {
            fail("mapping input");
        }
        std::slice::from_raw_parts(base as *const u8, bytes)
    }
}

/// Gives the pages of a region back (contents are lost or reloaded on the next touch).
#[allow(dead_code)]
pub fn release(start: *const u8, bytes: usize) {
    if bytes > 0 {
        // SAFETY: advice only; the range belongs to a mapping made by this module.
        unsafe {
            sys::advise(start as *mut c_void, bytes, sys::DROP_PAGES);
        }
    }
}

/// What the process has used so far.
pub struct Used {
    /// Processor seconds in user code and in the kernel.
    pub user: f64,
    pub kernel: f64,
    /// Page faults served without and with reading from disk.
    pub soft_faults: i64,
    pub hard_faults: i64,
}

pub fn used() -> Used {
    let mut raw = [0i64; 18];
    // SAFETY: the buffer is as large as the system's record (two time values and 14 counters).
    unsafe {
        sys::usage(0, &mut raw);
    }
    let secs = |s: i64, micros: i64| s as f64 + (micros as i32) as f64 / 1e6;
    Used { user: secs(raw[0], raw[1]), kernel: secs(raw[2], raw[3]), soft_faults: raw[8], hard_faults: raw[9] }
}

/// Ends the process at once: no destructors, no unwinding, no flushing.
pub fn quit(code: i32) -> ! {
    // SAFETY: plain process exit.
    unsafe { sys::leave(code) }
}

/// Hint that the cache line at `p` will be used soon. Never faults.
#[inline(always)]
pub fn soon<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: prefetch is a hint and accepts any address.
    unsafe {
        std::arch::x86_64::_mm_prefetch(p as *const i8, std::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: prefetch is a hint and accepts any address.
    unsafe {
        std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, preserves_flags, readonly));
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let _ = p;
}
