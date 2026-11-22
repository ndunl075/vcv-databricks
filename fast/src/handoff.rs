//! The hand-off: give up cleanly and `exec` the general binary.
//!
//! `give_up` removes every file this process may have written under `$OUTPUT_DIR`, then
//! replaces the process with the general pipeline (same arguments, same environment). It is
//! called when a precondition fails, from a panic hook, and from fatal-signal handlers.
//! Everything the signal path touches is prepared in advance: no allocation, no locks.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

extern "C" {
    fn execv(path: *const i8, argv: *const *const i8) -> i32;
    fn unlink(path: *const i8) -> i32;
    fn _exit(code: i32) -> !;
    fn sigprocmask(how: i32, set: *const u64, old: *mut u64) -> i32;
}

const MAX_PATHS: usize = 1024;
const PATH_LEN: usize = 320;

struct Registry {
    paths: std::cell::UnsafeCell<[[u8; PATH_LEN]; MAX_PATHS]>,
}
unsafe impl Sync for Registry {}
static REG: Registry = Registry { paths: std::cell::UnsafeCell::new([[0; PATH_LEN]; MAX_PATHS]) };
static REG_COUNT: AtomicUsize = AtomicUsize::new(0);
static REG_READY: [AtomicBool; MAX_PATHS] = [const { AtomicBool::new(false) }; MAX_PATHS];

static ABORTED: AtomicBool = AtomicBool::new(false);
static GIVING_UP: AtomicBool = AtomicBool::new(false);

struct ExecInfo {
    general: CString,
    argv: Vec<CString>,
    ptrs: Vec<*const i8>,
}
unsafe impl Sync for ExecInfo {}
static mut EXEC: Option<ExecInfo> = None;
static mut OUT_DIRS: Vec<std::path::PathBuf> = Vec::new();

/// True once a hand-off has started: writers must stop creating files.
pub fn aborted() -> bool {
    ABORTED.load(Ordering::Relaxed)
}

/// Records a path that is about to be created, so a hand-off can remove it.
/// Returns false if a hand-off is under way (do not create the file).
pub fn register_output(path: &std::path::Path) -> bool {
    if aborted() {
        return false;
    }
    let idx = REG_COUNT.fetch_add(1, Ordering::SeqCst);
    if idx < MAX_PATHS {
        let b = path.as_os_str().as_encoded_bytes();
        if b.len() < PATH_LEN {
            unsafe {
                let slot = &mut (*REG.paths.get())[idx];
                slot[..b.len()].copy_from_slice(b);
                slot[b.len()] = 0;
            }
            REG_READY[idx].store(true, Ordering::SeqCst);
        }
    }
    !aborted()
}

fn remove_registered() {
    let n = REG_COUNT.load(Ordering::SeqCst).min(MAX_PATHS);
    for i in 0..n {
        if REG_READY[i].load(Ordering::SeqCst) {
            unsafe {
                let slot = &(*REG.paths.get())[i];
                unlink(slot.as_ptr() as *const i8);
            }
        }
    }
}

/// Directory scan for files of ours (names start with `ssf-`); only from normal context.
fn remove_by_prefix() {
    unsafe {
        #[allow(static_mut_refs)]
        for d in OUT_DIRS.iter() {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with("ssf-") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
    }
}

/// Prepares the exec: the general binary (`$SSF_GENERAL`, default `/ss`) gets our arguments.
pub fn init(out_dir: &std::path::Path) {
    let general = std::env::var("SSF_GENERAL").unwrap_or_else(|_| "/ss".to_string());
    let general = CString::new(general).unwrap();
    let mut argv: Vec<CString> = vec![general.clone()];
    for a in std::env::args_os().skip(1) {
        use std::os::unix::ffi::OsStrExt;
        argv.push(CString::new(a.as_bytes()).unwrap());
    }
    let mut ptrs: Vec<*const i8> = argv.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    unsafe {
        EXEC = Some(ExecInfo { general, argv, ptrs });
        OUT_DIRS = vec![out_dir.join("user_daily"), out_dir.join("campaign_daily")];
    }
}

fn do_exec() -> ! {
    unsafe {
        // The signal being handled is blocked; the new image must not inherit that.
        let empty = [0u64; 16];
        sigprocmask(2, empty.as_ptr(), std::ptr::null_mut());
        #[allow(static_mut_refs)]
        if let Some(info) = EXEC.as_ref() {
            let _ = &info.argv;
            execv(info.general.as_ptr(), info.ptrs.as_ptr());
        }
        _exit(70)
    }
}

/// Give up: remove our output, exec the general binary. Never returns.
pub fn give_up(reason: &str) -> ! {
    if GIVING_UP.swap(true, Ordering::SeqCst) {
        // Another thread is already handing off; wait to be replaced.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    ABORTED.store(true, Ordering::SeqCst);
    eprintln!("ssf: hand-off to general pipeline: {reason}");
    remove_registered();
    remove_by_prefix();
    do_exec()
}

#[cfg(target_os = "linux")]
mod sig {
    use super::*;

    #[repr(C)]
    struct SigAction {
        handler: usize,
        mask: [u64; 16],
        flags: i32,
        restorer: usize,
    }
    #[repr(C)]
    struct StackT {
        sp: *mut u8,
        flags: i32,
        size: usize,
    }
    extern "C" {
        fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
        fn sigaltstack(ss: *const StackT, old: *mut StackT) -> i32;
    }
    const SA_ONSTACK: i32 = 0x0800_0000;
    const SA_NODEFER: i32 = 0x4000_0000;
    const SA_RESETHAND: i32 = 0x8000_0000u32 as i32;

    extern "C" fn on_fatal(_sig: i32) {
        // Async-signal-safe: unlink and execve only.
        if GIVING_UP.swap(true, Ordering::SeqCst) {
            loop {
                unsafe { _exit(71) }
            }
        }
        ABORTED.store(true, Ordering::SeqCst);
        remove_registered();
        do_exec()
    }

    pub fn install() {
        let act = SigAction { handler: on_fatal as usize, mask: [0; 16], flags: SA_ONSTACK | SA_NODEFER | SA_RESETHAND, restorer: 0 };
        for s in [4, 7, 8, 11] {
            // SIGILL, SIGBUS, SIGFPE, SIGSEGV
            unsafe { sigaction(s, &act, std::ptr::null_mut()) };
        }
        thread_setup();
    }

    /// Gives the calling thread an alternate signal stack (stack overflow still hands off).
    pub fn thread_setup() {
        const SZ: usize = 64 * 1024;
        let stack = Box::leak(vec![0u8; SZ].into_boxed_slice());
        let ss = StackT { sp: stack.as_mut_ptr(), flags: 0, size: SZ };
        unsafe { sigaltstack(&ss, std::ptr::null_mut()) };
    }
}

#[cfg(not(target_os = "linux"))]
mod sig {
    pub fn install() {}
    pub fn thread_setup() {}
}

pub use sig::thread_setup;

/// Installs the panic hook and (on Linux) fatal-signal handlers.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("panic: {info}");
        give_up(&msg)
    }));
    sig::install();
}
