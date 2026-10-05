//! AR-36: the crate's ELF constructors use no global allocator.
//! `proc-title`'s (`io::argv_title`, priority 100) keeps the arguments'
//! memory and copies the arguments into a block it maps; `startup-fds`'s
//! (`io::startup_fds`, priority 101) opens native Lean's startup
//! descriptors. Each runs before the program configures its allocator (a
//! translator's mimalloc options), so an allocation there would set the
//! allocator up early (lean2rr saw `main`'s first mimalloc arena made
//! without huge pages that way).
//!
//! This binary has a counting global allocator, and a constructor of its own
//! in plain `.init_array`, which runs after the crate's (the linkers put the
//! sections with a priority first) and records the count then: it must be 0.
//! The allocator also writes a mark on standard error at the first
//! allocation before that record, for the runs whose constructor ends the
//! process. The binary runs without libtest (`harness = false`), which
//! allocates before `main`; the checks run in child processes of itself:
//! - the plain start, then with `UV_USE_IO_URING=0` (set: the constructor
//!   reads it with `getenv`, where `std::env::var_os` would copy it): the
//!   count is 0; with `proc-title`, the title is written into the arguments'
//!   memory and the arguments stay as they were (the block of copies works);
//!   with `startup-fds`, the descriptors are open from 3 on, the epoll
//!   descriptor first, and with `UV_USE_IO_URING=0` there are 6 of them, as
//!   natively;
//! - with `startup-fds`, under `ulimit -n 4`, where the descriptors cannot
//!   be made: the constructor ends the process with Lean's internal panic
//!   line and status 1 (an abort with `LEAN_ABORT_ON_PANIC` set), and no
//!   allocation came first.
//!
//! Without either feature the crate has no constructor, and the count is 0
//! all the same.
//!
//! It counts the Rust global allocator only (the allocator a translator
//! configures, mimalloc in lean2rr), not libc's `malloc`: allocations that
//! glibc makes for its own calls (none in the crate's constructors today)
//! would not be seen (review RSH2-10).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

/// Allocations through the global allocator so far.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// The record of [`record`] is done.
static RECORDED: AtomicBool = AtomicBool::new(false);

/// The count when this binary's own constructor ran.
static AT_PROGRAM_CONSTRUCTOR: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The mark written at the first allocation before [`record`].
const MARK: &[u8] = b"ctor_alloc: allocation before the program's constructors\n";

struct Counting;

// SAFETY: every call is forwarded to `System`, which upholds the contract;
// the count and the mark are side effects that touch no allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded under the caller's contract (`ptr` came from
        // `System` through this allocator).
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Count an allocation; the first one before [`record`] writes [`MARK`].
fn note() {
    if ALLOCS.fetch_add(1, Relaxed) == 0 && !RECORDED.load(Relaxed) {
        let _ = rustix::io::write(rustix::stdio::stderr(), MARK);
    }
}

/// What descriptor 3 was when this binary's constructor ran.
static FD3_AT_PROGRAM_CONSTRUCTOR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// This binary's constructor: after the crate's (plain `.init_array`). It
/// records the count first, then (allocating, which no longer counts) what
/// descriptor 3 is, so a run can tell that the crate's constructor opened
/// the startup descriptors, not `ensure_native_descriptors` in `main`.
extern "C" fn record() {
    AT_PROGRAM_CONSTRUCTOR.store(ALLOCS.load(Relaxed), Relaxed);
    RECORDED.store(true, Relaxed);
    let fd3 = std::fs::read_link("/proc/self/fd/3").map(|l| l.display().to_string());
    *FD3_AT_PROGRAM_CONSTRUCTOR.lock().unwrap() = Some(fd3.unwrap_or_else(|e| e.to_string()));
}

#[used]
#[link_section = ".init_array"]
static RECORD: extern "C" fn() = record;

const CHILD: &str = "LEAN_RUNTIME_CTOR_ALLOC_CHILD";

/// A child run: what the constructors left.
fn child() {
    let at = AT_PROGRAM_CONSTRUCTOR.load(Relaxed);
    println!("allocations before the program's constructors: {at}");
    let fd3 = FD3_AT_PROGRAM_CONSTRUCTOR.lock().unwrap().clone();
    println!(
        "fd 3 at the program's constructors: {}",
        fd3.unwrap_or_default()
    );
    // what a glue calls at `main`'s start, which also keeps the crate's
    // constructors linked (a `#[used]` static alone does not keep its object
    // file in an rlib)
    lean_runtime::io::startup::mark_end_initialization();
    #[cfg(feature = "startup-fds")]
    lean_runtime::io::startup::ensure_native_descriptors();
    // below 9: with `UV_USE_IO_URING=0` the startup descriptors are 3 to 8,
    // and the directory's own descriptor is then 9
    let mut fds: Vec<u32> = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|&n| n < 9)
        .collect();
    fds.sort();
    let fds: Vec<String> = fds.iter().map(u32::to_string).collect();
    println!("fds below 9: {}", fds.join(" "));
    let fd3 = std::fs::read_link("/proc/self/fd/3").map(|l| l.display().to_string());
    println!("fd 3: {}", fd3.unwrap_or_else(|e| e.to_string()));
    #[cfg(feature = "proc-title")]
    {
        let args: Vec<String> = std::env::args().collect();
        let title = "ctor-alloc";
        lean_runtime::io::uvsys::set_process_title(title.as_bytes()).unwrap();
        let cmdline = std::fs::read("/proc/self/cmdline").unwrap();
        println!(
            "title written: {}",
            cmdline.starts_with(title.as_bytes()) && cmdline[title.len()..].iter().all(|&b| b == 0)
        );
        println!(
            "args unchanged: {}",
            std::env::args().collect::<Vec<_>>() == args
        );
    }
}

/// Run this binary as a child through `sh -c "<prefix>; exec ..."`, with the
/// environment `env` (and `UV_USE_IO_URING`, `LEAN_ABORT_ON_PANIC`
/// removed otherwise). Descriptors 3 to 9 that the test runner may have
/// left open (a jobserver's) are closed first, so the child starts with 0,
/// 1 and 2 as a program does.
fn run(prefix: &str, env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    let mut c = std::process::Command::new("sh");
    c.arg("-c")
        .arg(format!(
            "exec 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-; {prefix}; exec \"$0\" child-arg"
        ))
        .arg(exe)
        .env(CHILD, "1")
        .env_remove("UV_USE_IO_URING")
        .env_remove("LEAN_ABORT_ON_PANIC");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn check_start(env: &[(&str, &str)], descriptors: Option<usize>) {
    let out = run(":", env);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{env:?}: {out:?}");
    assert!(!stderr.contains("ctor_alloc:"), "{env:?}: {stderr}");
    assert!(
        stdout.contains("allocations before the program's constructors: 0\n"),
        "{env:?}: {stdout}"
    );
    if cfg!(feature = "proc-title") {
        assert!(
            stdout.contains("title written: true\n"),
            "{env:?}: {stdout}"
        );
        assert!(
            stdout.contains("args unchanged: true\n"),
            "{env:?}: {stdout}"
        );
    }
    if cfg!(feature = "startup-fds") {
        assert!(
            stdout.contains("fd 3 at the program's constructors: anon_inode:[eventpoll]\n"),
            "{env:?}: {stdout}"
        );
        assert!(
            stdout.contains("fd 3: anon_inode:[eventpoll]\n"),
            "{env:?}: {stdout}"
        );
        if let Some(n) = descriptors {
            let want: Vec<String> = (0..3 + n).map(|i| i.to_string()).collect();
            assert!(
                stdout.contains(&format!("fds below 9: {}\n", want.join(" "))),
                "{env:?}: {stdout}"
            );
        }
    }
    println!("ctor_alloc: start {env:?}: ok");
}

#[cfg(feature = "startup-fds")]
fn check_failure(abort: bool) {
    use std::os::unix::process::ExitStatusExt;
    let env: &[(&str, &str)] = if abort {
        &[("LEAN_ABORT_ON_PANIC", "1")]
    } else {
        &[]
    };
    let out = run("ulimit -n 4", env);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "INTERNAL PANIC: Failed to initialize event loop: too many open files\n",
        "{out:?}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
    if abort {
        assert_eq!(out.status.signal(), Some(6), "{out:?}");
    } else {
        assert_eq!(out.status.code(), Some(1), "{out:?}");
    }
    println!("ctor_alloc: failure (abort {abort}): ok");
}

fn main() {
    if std::env::var_os(CHILD).is_some() {
        child();
        return;
    }
    if cfg!(miri) {
        return;
    }
    check_start(&[], None);
    // libuv 1.48 with `UV_USE_IO_URING=0`: no ring, 6 descriptors
    check_start(&[("UV_USE_IO_URING", "0")], Some(6));
    #[cfg(feature = "startup-fds")]
    {
        check_failure(false);
        check_failure(true);
    }
}
