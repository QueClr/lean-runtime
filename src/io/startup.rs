//! The descriptors native Lean has open before `main` (A821), and
//! `IO.initializing`'s flag ([`initializing`]).
//!
//! Lean's runtime starts libuv's default loop during initialization
//! (`initialize_libuv`, then `event_loop_init` in `src/runtime/uv/event_loop.cpp`),
//! and libuv 1.48.0, the version Lean 4.34.0 links, opens, close-on-exec, at
//! the lowest free numbers, in this order (`uv_loop_init`):
//! 1. an epoll descriptor (`uv__platform_loop_init`);
//! 2. two io_uring rings, the one polled by a kernel thread (64 entries,
//!    `IORING_SETUP_SQPOLL`) and the control ring (256 entries), when
//!    `uv__use_io_uring` says so and `io_uring_setup` gives rings with the
//!    features libuv needs (`uv__iou_init`); the polling ring is added to the
//!    epoll descriptor;
//! 3. the blocking pipe that locks signal handling, with one byte written
//!    into it (`uv__signal_global_once_init`);
//! 4. the loop's non-blocking signal pipe (`uv__process_init`), through
//!    which `sched::uv`'s signal watchers are woken, as libuv's are;
//! 5. an eventfd, non-blocking (the loop's async handle).
//!
//! Before `main`, a native Lean program also ignores `SIGPIPE`
//! (`initialize_io`, io.cpp:1668: `signal(SIGPIPE, SIG_IGN)`), so a write to
//! a pipe without a reader fails with `EPIPE` (an `IO.Error`) instead of
//! killing the process.
//!
//! **The translator's glue duties** at startup (review RIO1-03):
//! - ignore `SIGPIPE` before Lean code runs: Rust's `lang_start` does it for a
//!   Rust `main`; an entry that is not `lang_start` (lean2rr's) must do it
//!   itself;
//! - run an ELF constructor (`#[link_section = ".init_array"]`) that calls
//!   [`open_native_descriptors`], and on `Err` [`fail_as_native`];
//! - call [`mark_end_initialization`] once the module initializers have run,
//!   before `main`, as the generated `main` calls
//!   `lean_io_mark_end_initialization` (also when an initializer failed).
//!
//! The crate does none of them itself: its ELF constructors live only in
//! the native quirks' files (the process's arguments, [`super::argv_title`],
//! need no glue), `SIGPIPE`'s disposition belongs to the entry, and only the
//! glue knows when the initializers end.
//!
//! So on a host with io_uring, descriptors 3 to 10 are taken, and a standard
//! descriptor closed at startup is taken by the first of them: reading a
//! closed stdin or writing a closed stdout then fails with `EINVAL`, and the
//! point where opening a file fails with `EMFILE` is native's. A translator
//! calls [`open_native_descriptors`] from an ELF constructor, before Rust's
//! runtime puts `/dev/null` in the place of closed standard descriptors (the
//! constructor needs `#[link_section]`, which `deny(unsafe_code)` refuses,
//! so it lives in each translator's glue).
//!
//! The rings are real io_uring rings, made as libuv 1.48.0's `uv__iou_init`
//! makes them, through the io-uring crate (tokio-rs; accepted by leanrs's
//! shared-runtime coordinator under the owner's delegation of dependency
//! decisions (2026-10-04); `UNSAFE.md`, "`unsafe` in dependencies"):
//! - libuv's choice first ([`use_io_uring`]: the kernel version,
//!   `UV_USE_IO_URING`);
//! - then `io_uring_setup` with libuv's parameters: 64 entries with
//!   `IORING_SETUP_SQPOLL` and `sq_thread_idle = 10` (milliseconds), then
//!   256 entries with no flag, every other parameter zero (never
//!   `IORING_SETUP_ATTACH_WQ`); the kernel gives each descriptor
//!   close-on-exec, at the lowest free number;
//! - the rings' memory mapped as libuv maps it (the submission and
//!   completion rings in one map, the submission entries in another,
//!   `MAP_SHARED | MAP_POPULATE`), so `/proc/self/maps` shows native's four
//!   `anon_inode:[io_uring]` mappings;
//! - a ring kept only with `IORING_FEAT_RSRC_TAGS`, `SINGLE_MMAP` and
//!   `NODROP` (libuv checks before it maps, the crate after; either way
//!   such a ring ends unmapped and closed);
//! - the polling ring added to the epoll descriptor (`POLLIN`, data its
//!   number), as `uv__iou_init` adds it; if that fails, the ring is closed.
//!
//! So the kernel decides, as natively, whether each ring exists
//! (`kernel.io_uring_disabled`, a seccomp filter, a user namespace, a
//! kernel before 5.13), and `fstat` (a ring's own inode), `/proc/self/fdinfo`
//! (the rings' sizes, the polling thread), `/proc/self/maps` and
//! `/proc/self/task` (the ring's kernel thread, `iou-sqp-<pid>`) show
//! native's rings (case `io/startup_rings`). The rings are kept in
//! `DESCRIPTORS`, a `static`, so they are never dropped (their `Drop` would
//! unmap and close them, as libuv's `uv__iou_delete`).
//!
//! What still differs: native's loop thread starts the loop at once and
//! stays in it, blocked in `epoll_wait`. Its first iteration adds the signal
//! pipe and the eventfd to the epoll descriptor, through two `EPOLL_CTL`
//! submissions on the control ring, whose `/proc/self/fdinfo` then shows
//! `SqHead`, `SqTail`, `CachedSqHead`, `CqHead`, `CqTail` and `CachedCqTail`
//! 2 (0 here), and that thread is a task of its own. Here the epoll
//! descriptor lists the polling ring and what the scheduler's event loop
//! registers (the signal pipe once a signal watcher listens), and the
//! eventfd only while a DNS lookup of `net` is pending (its helpers wake the
//! loop through it, and the loop drains it): registering a readable
//! descriptor that nothing drains would wake the loop for good. The polling ring's watch never fires: nothing is
//! submitted to it, so it has no completion.
//!
//! Source: lean2rr's `runtime/leanrt/src/rt.rs` (`reserve_libuv_descriptors`,
//! `kernel_version`), rewritten over rustix's safe API and the io-uring
//! crate's. lean2rr's version
//! makes the control ring even when `uv__use_io_uring` says no, which libuv
//! 1.48.0 does not (`UV_USE_IO_URING=0` gives 6 descriptors natively).

use io_uring::IoUring;
use rustix::event::{epoll, eventfd, EventfdFlags};
#[cfg(feature = "sched")]
use rustix::fd::{AsFd, BorrowedFd};
use rustix::fd::{AsRawFd, OwnedFd};
use rustix::pipe::{pipe_with, PipeFlags};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Lean's `g_initializing` (io.cpp): true from the start of the process until
/// the glue's [`mark_end_initialization`].
static INITIALIZING: AtomicBool = AtomicBool::new(true);

/// `IO.initializing` (`lean_io_initializing`): true while the module
/// initializers run (from the start of the process), false once the glue has
/// called [`mark_end_initialization`], so in `main` and in every task.
pub fn initializing() -> bool {
    INITIALIZING.load(Ordering::Relaxed)
}

/// `lean_io_mark_end_initialization`: the generated `main` calls it right
/// after the module initializers (whether they succeeded or not), before
/// `main` runs; so does each translator's glue.
pub fn mark_end_initialization() {
    INITIALIZING.store(false, Ordering::Relaxed)
}

/// Why native Lean's startup would not reach `main`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupFailure {
    /// `uv_loop_init` failed (the epoll descriptor, the signal pipe or the
    /// eventfd could not be made): `uv_default_loop` returns NULL and
    /// `event_loop_init`'s `uv_async_init` on it crashes (SIGSEGV).
    LoopInit,
    /// The signal lock pipe could not be made or written: libuv calls
    /// `abort()` (`uv__signal_global_reinit`).
    SignalLock,
}

/// The descriptors, kept open for the life of the process, as libuv keeps
/// them.
struct Descriptors {
    #[cfg_attr(not(feature = "sched"), allow(dead_code))]
    epoll: OwnedFd,
    _rings: Vec<IoUring>,
    _lock_pipe: (OwnedFd, OwnedFd),
    /// Kept open for the life of the process, never closed, `dup2`'d over or
    /// reused: signal-hook's handlers write to its write end by number
    /// (`sched::uv`; docs/sched.md, "Std.Internal.UV").
    #[cfg_attr(not(feature = "sched"), allow(dead_code))]
    signal_pipe: (OwnedFd, OwnedFd),
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    eventfd: OwnedFd,
}

static DESCRIPTORS: OnceLock<Result<Descriptors, StartupFailure>> = OnceLock::new();

/// Open native Lean's startup descriptors, once (later calls return the first
/// outcome). On `Err`, native Lean would not reach `main`: the translator
/// ends the process as [`fail_as_native`] does.
pub fn open_native_descriptors() -> Result<(), StartupFailure> {
    match DESCRIPTORS.get_or_init(open_all) {
        Ok(_) => Ok(()),
        Err(f) => Err(*f),
    }
}

/// libuv's loop descriptor (an epoll instance), for the scheduler's event
/// loop (`sched`'s reactor), once [`open_native_descriptors`] has opened
/// it: the first caller gets it, so one scheduler registers its descriptors
/// there, as libuv's one loop does; later callers make their own.
#[cfg(feature = "sched")]
pub(crate) fn claim_loop_epoll() -> Option<BorrowedFd<'static>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CLAIMED: AtomicBool = AtomicBool::new(false);
    match DESCRIPTORS.get() {
        Some(Ok(d)) if !CLAIMED.swap(true, Ordering::Relaxed) => Some(d.epoll.as_fd()),
        _ => None,
    }
}

/// The read and write ends of the loop's signal pipe, for `sched::uv`'s
/// signal watchers, once [`open_native_descriptors`] has opened them: the
/// first caller gets them, as `claim_loop_epoll`, so the watchers open no
/// descriptor of their own, as natively (review RSIOB-05). The pipe lives
/// in this module's static for the life of the process: the claimer may
/// register its write end with signal handlers by number.
#[cfg(feature = "sched")]
pub(crate) fn claim_signal_pipe() -> Option<(BorrowedFd<'static>, BorrowedFd<'static>)> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CLAIMED: AtomicBool = AtomicBool::new(false);
    match DESCRIPTORS.get() {
        Some(Ok(d)) if !CLAIMED.swap(true, Ordering::Relaxed) => {
            Some((d.signal_pipe.0.as_fd(), d.signal_pipe.1.as_fd()))
        }
        _ => None,
    }
}

/// The loop's async eventfd (`loop->async_io_watcher`), once
/// [`open_native_descriptors`] has opened it: libuv's thread pool wakes the
/// loop through it, and so do `net`'s DNS helpers.
#[cfg(feature = "net")]
pub(crate) fn loop_eventfd() -> Option<BorrowedFd<'static>> {
    match DESCRIPTORS.get() {
        Some(Ok(d)) => Some(d.eventfd.as_fd()),
        _ => None,
    }
}

fn open_all() -> Result<Descriptors, StartupFailure> {
    let cloexec = epoll::CreateFlags::CLOEXEC;
    let epoll = epoll::create(cloexec).map_err(|_| StartupFailure::LoopInit)?;
    let mut rings = Vec::new();
    if use_io_uring() {
        // `uv__iou_init` twice; a ring the kernel does not give is skipped.
        // The polling ring is watched for completions (`POLLIN`, data its
        // number); if that fails, libuv closes it.
        if let Some(ring) = ring(POLLING_ENTRIES, true) {
            let data = epoll::EventData::new_u64(ring.as_raw_fd() as u64);
            if epoll::add(&epoll, &ring, data, epoll::EventFlags::IN).is_ok() {
                rings.push(ring);
            }
        }
        if let Some(ring) = ring(CONTROL_ENTRIES, false) {
            rings.push(ring);
        }
    }
    let lock_pipe = pipe_with(PipeFlags::CLOEXEC).map_err(|_| StartupFailure::SignalLock)?;
    // `uv__signal_unlock`
    rustix::io::write(&lock_pipe.1, &[42]).map_err(|_| StartupFailure::SignalLock)?;
    let signal_pipe = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)
        .map_err(|_| StartupFailure::LoopInit)?;
    let eventfd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
        .map_err(|_| StartupFailure::LoopInit)?;
    Ok(Descriptors {
        epoll,
        _rings: rings,
        _lock_pipe: lock_pipe,
        signal_pipe,
        eventfd,
    })
}

/// The polling ring's entries (`uv__platform_loop_init`).
const POLLING_ENTRIES: u32 = 64;

/// The control ring's entries.
const CONTROL_ENTRIES: u32 = 256;

/// `uv__iou_init`'s ring: `io_uring_setup` of `entries` entries, polled by a
/// kernel thread when `polling` (`IORING_SETUP_SQPOLL`, `sq_thread_idle =
/// 10`), every other parameter zero, the memory mapped; kept only with the
/// features libuv needs. `None` where libuv keeps no ring (the setup or a
/// map fails, or a feature is missing: the ring is then unmapped and
/// closed).
fn ring(entries: u32, polling: bool) -> Option<IoUring> {
    let mut builder = IoUring::builder();
    if polling {
        builder.setup_sqpoll(10);
    }
    let ring: IoUring = builder.build(entries).ok()?;
    let p = ring.params();
    (p.is_feature_resource_tagging() && p.is_feature_single_mmap() && p.is_feature_nodrop())
        .then_some(ring)
}

/// End the process as native Lean's startup does on `failure`: `abort()`
/// (SIGABRT) for the signal lock, a crash by SIGSEGV for a failed loop.
/// SIGSEGV is raised twice: Rust's std installs a SIGSEGV handler (its stack
/// overflow report) that, for a fault outside a guard page, restores the
/// default action and returns, so the second raise kills the process (review
/// RIO1-08); `abort()` only if a handler returns from both.
pub fn fail_as_native(failure: StartupFailure) -> ! {
    if failure == StartupFailure::LoopInit {
        for _ in 0..2 {
            let _ = nix::sys::signal::raise(nix::sys::signal::Signal::SIGSEGV);
        }
    }
    std::process::abort()
}

/// libuv 1.48.0's `uv__use_io_uring`: whether `uv__iou_init` tries to make
/// its rings at all: on Linux 5.10.186 or later by [`kernel_version`], or as
/// `UV_USE_IO_URING` says (non-zero by `atoi` is yes). Whether a ring then
/// exists is the kernel's answer to `io_uring_setup`
/// (`ring`). libuv's branches for other targets (always
/// no on Android, 32-bit ARM and powerpc64; 6.1.51 on hppa) do not apply:
/// `io` builds only for Linux on x86-64 and aarch64, both little-endian, so
/// the epoll watch's data is also libuv's `data.fd` (review RQ2-06).
pub fn use_io_uring() -> bool {
    let mut use_io_uring = kernel_version() >= 0x05_0A_BA;
    if let Some(v) = std::env::var_os("UV_USE_IO_URING") {
        use_io_uring = atoi(std::os::unix::ffi::OsStrExt::as_bytes(v.as_os_str())) != 0;
    }
    use_io_uring
}

/// glibc's `atoi`, `(int) strtol(s, NULL, 10)`: leading white space, a
/// sign, decimal digits, saturated at `LONG_MAX` or `LONG_MIN` (64 bits)
/// like `strtol`, then cut to `int` (the low 32 bits) like the cast (review
/// RQ2-05).
fn atoi(s: &[u8]) -> i32 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = i < s.len() && s[i] == b'-';
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    // accumulated with its sign, so that `LONG_MIN` itself is reached
    let mut v: Option<i64> = Some(0);
    while i < s.len() && s[i].is_ascii_digit() {
        let d = i64::from(s[i] - b'0');
        v = v.and_then(|v| v.checked_mul(10)).and_then(|v| {
            if neg {
                v.checked_sub(d)
            } else {
                v.checked_add(d)
            }
        });
        i += 1;
    }
    let long = v.unwrap_or(if neg { i64::MIN } else { i64::MAX });
    long as i32
}

/// `sscanf("%u.%u.%u")` at the start of `s` (after white space).
fn three_numbers(s: &[u8]) -> Option<(u32, u32, u32)> {
    let mut i = 0;
    let mut out = [0u32; 3];
    for (k, slot) in out.iter_mut().enumerate() {
        while i < s.len() && s[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < s.len() && s[i].is_ascii_digit() {
            *slot = slot.wrapping_mul(10).wrapping_add((s[i] - b'0') as u32);
            i += 1;
        }
        if i == start {
            return None;
        }
        if k < 2 {
            if i >= s.len() || s[i] != b'.' {
                return None;
            }
            i += 1;
        }
    }
    Some((out[0], out[1], out[2]))
}

/// libuv 1.48.0's `uv__kernel_version`: the mainline version Ubuntu gives in
/// `/proc/version_signature` (`Ubuntu <kernel> <a.b.c>`), else the one in a
/// Debian `uname` version (`... Debian a.b.c ...`), else `uname`'s release,
/// with the `UNAME26` personality's 2.6.x mapped back; as `a * 65536 + b * 256
/// + c`, 0 when unknown.
pub fn kernel_version() -> u32 {
    let mk = |(a, b, c): (u32, u32, u32)| {
        a.wrapping_mul(65536)
            .wrapping_add(b.wrapping_mul(256))
            .wrapping_add(c)
    };
    if let Ok(sig) = std::fs::read("/proc/version_signature") {
        let sig = &sig[..sig.len().min(255)];
        let mut words = sig
            .split(|b| b.is_ascii_whitespace())
            .filter(|w| !w.is_empty());
        if words.next() == Some(&b"Ubuntu"[..]) && words.next().is_some() {
            if let Some(v) = words.next().and_then(three_numbers) {
                return mk(v);
            }
        }
    }
    let u = rustix::system::uname();
    let version = u.version().to_bytes();
    if let Some(at) = version.windows(7).position(|w| w == b"Debian ") {
        if let Some(v) = three_numbers(&version[at + 7..]) {
            return mk(v);
        }
    }
    let Some((mut a, mut b, mut c)) = three_numbers(u.release().to_bytes()) else {
        return 0;
    };
    if a == 2 && b == 6 {
        if c >= 60 {
            (a, b, c) = (4, c - 60, 0);
        } else if c >= 40 {
            (a, b, c) = (3, c - 40, 0);
        }
    }
    mk((a, b, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only test that ends initialization: true until then, false after.
    #[test]
    fn initializing_until_marked() {
        assert!(initializing());
        mark_end_initialization();
        assert!(!initializing());
        mark_end_initialization();
        assert!(!initializing());
    }

    /// The value of `key` in the ring's `/proc/self/fdinfo` text.
    fn fdinfo(fd: &IoUring, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).ok()?;
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k == key).then(|| v.trim().to_owned())
        })
    }

    /// Both rings as libuv makes them: the parameters libuv passes, their
    /// sizes (`SqMask` is the entries less one, `CqMask` twice the entries
    /// less one) and no polling thread for the control ring, where the
    /// kernel's `/proc/self/fdinfo` shows them (Linux 5.16 or later);
    /// close-on-exec; the polling thread, an io_uring kernel thread of this
    /// process. Where the kernel gives no ring (io_uring disabled, a kernel
    /// before 5.13), nothing is checked.
    #[test]
    #[cfg_attr(miri, ignore)] // Miri has no `io_uring_setup`
    fn rings_are_libuvs() {
        let (Some(polling), Some(control)) =
            (ring(POLLING_ENTRIES, true), ring(CONTROL_ENTRIES, false))
        else {
            eprintln!("no io_uring ring on this host: nothing checked");
            return;
        };
        // the parameters: SQPOLL for the polling ring only, with libuv's
        // idle time, and the sizes asked for
        assert!(polling.params().is_setup_sqpoll());
        assert!(!control.params().is_setup_sqpoll());
        assert_eq!(polling.params().sq_entries(), POLLING_ENTRIES);
        assert_eq!(control.params().sq_entries(), CONTROL_ENTRIES);
        // a kernel whose fdinfo lacks a line skips its check; the polling
        // ring's `SqThread` can read -1 while its thread is busy on some
        // 6.6-era kernels, so the thread is checked below instead
        for (fd, key, want) in [
            (&polling, "SqMask", "0x3f"),
            (&polling, "CqMask", "0x7f"),
            (&control, "SqMask", "0xff"),
            (&control, "CqMask", "0x1ff"),
            (&control, "SqThread", "-1"),
        ] {
            if let Some(v) = fdinfo(fd, key) {
                assert_eq!(v, want, "{key}");
            }
        }
        for fd in [&polling, &control] {
            let flags = rustix::io::fcntl_getfd(fd).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        }
        // the polling thread is a task of this process, an io_uring kernel
        // thread (`PF_IO_WORKER` in its `stat` flags). Its name,
        // `iou-sqp-<pid>`, and `SqThread`, are set only once it has run,
        // which may be after this line, so the tasks' flags are scanned.
        let io_worker = |t: &std::path::Path| {
            std::fs::read(t.join("stat")).is_ok_and(|stat| {
                let rest = &stat[stat.iter().rposition(|&b| b == b')').unwrap_or(0)..];
                let flags = rest
                    .split(|b| b.is_ascii_whitespace())
                    .filter(|f| !f.is_empty())
                    .nth(7);
                flags
                    .and_then(|f| std::str::from_utf8(f).ok()?.parse::<u64>().ok())
                    .is_some_and(|f| f & 0x10 != 0)
            })
        };
        let polls = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .any(|t| t.is_ok_and(|t| io_worker(&t.path())));
        assert!(polls);
    }

    #[test]
    fn atoi_is_c_atoi() {
        assert_eq!(atoi(b"1"), 1);
        assert_eq!(atoi(b"0"), 0);
        assert_eq!(atoi(b" -2x"), -2);
        assert_eq!(atoi(b"abc"), 0);
        assert_eq!(atoi(b""), 0);
        assert_eq!(atoi(b"+07"), 7);
        // `strtol` saturates, then the cast to `int` keeps the low 32 bits
        assert_eq!(atoi(b"2147483648"), i32::MIN);
        assert_eq!(atoi(b"4294967296"), 0);
        assert_eq!(atoi(b"9223372036854775807"), -1);
        assert_eq!(atoi(b"9223372036854775808"), -1);
        assert_eq!(atoi(b"18446744073709551616"), -1);
        assert_eq!(atoi(b"-9223372036854775808"), 0);
        assert_eq!(atoi(b"-9223372036854775809"), 0);
        assert_eq!(atoi(b"-4294967297"), -1);
    }

    #[test]
    fn sscanf_three_numbers() {
        assert_eq!(three_numbers(b"7.0.14\n"), Some((7, 0, 14)));
        assert_eq!(three_numbers(b"5.10.186-generic"), Some((5, 10, 186)));
        assert_eq!(three_numbers(b"6.1"), None);
        assert_eq!(three_numbers(b"x6.1.2"), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn kernel_version_is_unames_or_ubuntus() {
        let u = rustix::system::uname();
        let (a, b, _) = three_numbers(u.release().to_bytes()).unwrap();
        let kv = kernel_version();
        // the mainline version Ubuntu reports keeps uname's major and minor
        assert_eq!((kv >> 16, (kv >> 8) & 0xff), (a, b));
    }
}
