//! The descriptors native Lean has open before `main` (A821).
//!
//! Lean's runtime starts libuv's default loop during initialization
//! (`initialize_libuv`, then `event_loop_init` in `src/runtime/uv/event_loop.cpp`),
//! and libuv 1.48.0, the version Lean 4.34.0 links, opens, close-on-exec, at
//! the lowest free numbers, in this order (`uv_loop_init`):
//! 1. an epoll descriptor (`uv__platform_loop_init`);
//! 2. two io_uring rings, the one polled by a kernel thread (64 entries,
//!    `IORING_SETUP_SQPOLL`) and the control ring (256 entries), when
//!    `uv__use_io_uring` says so and `io_uring_setup` gives rings with the
//!    features libuv needs (`uv__iou_init`);
//! 3. the blocking pipe that locks signal handling, with one byte written
//!    into it (`uv__signal_global_once_init`);
//! 4. the loop's non-blocking signal pipe (`uv__process_init`);
//! 5. an eventfd, non-blocking (the loop's async handle).
//!
//! Before `main`, a native Lean program also ignores `SIGPIPE`
//! (`initialize_io`, io.cpp:1668: `signal(SIGPIPE, SIG_IGN)`), so a write to
//! a pipe without a reader fails with `EPIPE` (an `IO.Error`) instead of
//! killing the process.
//!
//! **The translator's glue duties** (review RIO1-03), neither expressible in
//! this crate's safe code:
//! - ignore `SIGPIPE` before Lean code runs: Rust's `lang_start` does it for a
//!   Rust `main`; an entry that is not `lang_start` (lean2rr's) must do it
//!   itself;
//! - run an ELF constructor (`#[link_section = ".init_array"]`) that calls
//!   [`open_native_descriptors`], and on `Err` [`fail_as_native`].
//!
//! So on a host with io_uring, descriptors 3 to 10 are taken, and a standard
//! descriptor closed at startup is taken by the first of them: reading a
//! closed stdin or writing a closed stdout then fails with `EINVAL`, and the
//! point where opening a file fails with `EMFILE` is native's. A translator
//! calls [`open_native_descriptors`] from an ELF constructor, before Rust's
//! runtime puts `/dev/null` in the place of closed standard descriptors (the
//! constructor needs `#[link_section]`, which `forbid(unsafe_code)` refuses,
//! so it lives in each translator's glue).
//!
//! The rings are stood in for by two more epoll descriptors: no safe API
//! creates a ring without a new crate (rustix's `io_uring_setup` is
//! `unsafe`). On every descriptor operation a Lean program can reach, an
//! io_uring descriptor and an epoll descriptor fail alike (`read` and `write`
//! give `EINVAL`; checked natively, `work/io-1-a821/FINDING.txt`). What differs:
//! - `fstat` (`System.FilePath.metadata "/proc/self/fd/4"`): a ring has its
//!   own inode, timestamped when it was made; an epoll descriptor shares the
//!   anonymous inode, timestamped at boot;
//! - `/proc/self/task` lists the ring's kernel thread (`iou-sqp-<pid>`)
//!   natively, not here.
//!
//! Whether libuv makes the rings is decided as libuv 1.48.0 decides it
//! ([`io_uring_rings_expected`]), plus what makes `io_uring_setup` fail on
//! purpose (`kernel.io_uring_disabled`). Known gaps, where native has two
//! descriptors fewer than predicted:
//! - a seccomp filter that refuses `io_uring_setup` (some container runtimes
//!   install one): it cannot be read reliably;
//! - `kernel.io_uring_disabled = 1` in a user namespace (rootless
//!   containers): the kernel checks `CAP_SYS_ADMIN` in the initial user
//!   namespace, while the effective capabilities in `/proc/self/status` are
//!   the namespace's own (review RIO1-10).
//!
//! Source: lean2rr's `runtime/leanrt/src/rt.rs` (`reserve_libuv_descriptors`,
//! `kernel_version`), rewritten over rustix's safe API. lean2rr's version
//! makes the control ring even when `uv__use_io_uring` says no, which libuv
//! 1.48.0 does not (`UV_USE_IO_URING=0` gives 6 descriptors natively).

use rustix::event::{epoll, eventfd, EventfdFlags};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::pipe::{pipe_with, PipeFlags};
use std::sync::OnceLock;

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
#[derive(Debug)]
struct Descriptors {
    _epoll: OwnedFd,
    _rings: Vec<OwnedFd>,
    _lock_pipe: (OwnedFd, OwnedFd),
    signal_pipe: (OwnedFd, OwnedFd),
    _eventfd: OwnedFd,
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

/// The read and write ends of the loop's signal pipe, for signal watchers,
/// once [`open_native_descriptors`] has opened them.
pub fn signal_pipe() -> Option<(BorrowedFd<'static>, BorrowedFd<'static>)> {
    match DESCRIPTORS.get() {
        Some(Ok(d)) => Some((d.signal_pipe.0.as_fd(), d.signal_pipe.1.as_fd())),
        _ => None,
    }
}

fn open_all() -> Result<Descriptors, StartupFailure> {
    let cloexec = epoll::CreateFlags::CLOEXEC;
    let epoll = epoll::create(cloexec).map_err(|_| StartupFailure::LoopInit)?;
    let mut rings = Vec::new();
    if io_uring_rings_expected() {
        // `uv__iou_init` twice; a ring that cannot be made is skipped
        for _ in 0..2 {
            if let Ok(fd) = epoll::create(cloexec) {
                rings.push(fd);
            }
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
        _epoll: epoll,
        _rings: rings,
        _lock_pipe: lock_pipe,
        signal_pipe,
        _eventfd: eventfd,
    })
}

/// End the process as native Lean's startup does on `failure`: `abort()`
/// (SIGABRT) for the signal lock, a crash by SIGSEGV for a failed loop
/// (raised here; if a handler returns from it, `abort()`).
pub fn fail_as_native(failure: StartupFailure) -> ! {
    if failure == StartupFailure::LoopInit {
        let _ = nix::sys::signal::raise(nix::sys::signal::Signal::SIGSEGV);
    }
    std::process::abort()
}

/// Whether libuv 1.48.0 makes its two io_uring rings on this host:
/// `uv__use_io_uring` (Linux 5.10.186 or later by [`kernel_version`], or as
/// `UV_USE_IO_URING` says: non-zero by `atoi` is yes), and `io_uring_setup`
/// succeeding with `IORING_FEAT_RSRC_TAGS` (Linux 5.13 or later), which
/// `kernel.io_uring_disabled` forbids (2: for everyone; 1: unless the process
/// has `CAP_SYS_ADMIN` or is in `kernel.io_uring_group`).
pub fn io_uring_rings_expected() -> bool {
    let kv = kernel_version();
    let mut use_io_uring = kv >= 0x05_0A_BA;
    if let Some(v) = std::env::var_os("UV_USE_IO_URING") {
        use_io_uring = atoi(std::os::unix::ffi::OsStrExt::as_bytes(v.as_os_str())) != 0;
    }
    use_io_uring && kv >= 0x05_0D_00 && io_uring_allowed()
}

/// C's `atoi`: leading white space, a sign, decimal digits (wrapping).
fn atoi(s: &[u8]) -> i32 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = i < s.len() && s[i] == b'-';
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    let mut v: i32 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add((s[i] - b'0') as i32);
        i += 1;
    }
    if neg {
        v.wrapping_neg()
    } else {
        v
    }
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

/// The io_uring part of `io_uring_allowed` (Linux 6.6 and later; earlier
/// kernels have no such setting): `kernel.io_uring_disabled` 0 allows, 2
/// forbids, 1 allows a process with `CAP_SYS_ADMIN` or in
/// `kernel.io_uring_group`. A setting that cannot be read allows. The
/// capability is read from `CapEff`, the process's own user namespace, while
/// the kernel asks the initial one: a known gap in user namespaces.
fn io_uring_allowed() -> bool {
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
    };
    match read("/proc/sys/kernel/io_uring_disabled") {
        None | Some(0) => true,
        Some(1) => {
            let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
                return true;
            };
            let field = |name: &str| {
                status
                    .lines()
                    .find_map(|l| l.strip_prefix(name))
                    .unwrap_or("")
            };
            let cap_eff = u64::from_str_radix(field("CapEff:").trim(), 16).unwrap_or(0);
            if cap_eff & (1 << 21) != 0 {
                return true;
            }
            let Some(group) = read("/proc/sys/kernel/io_uring_group").filter(|&g| g >= 0) else {
                return false;
            };
            let fsgid = field("Gid:")
                .split_whitespace()
                .nth(3)
                .and_then(|g| g.parse::<i64>().ok());
            fsgid == Some(group)
                || field("Groups:")
                    .split_whitespace()
                    .any(|g| g.parse::<i64>() == Ok(group))
        }
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_is_c_atoi() {
        assert_eq!(atoi(b"1"), 1);
        assert_eq!(atoi(b"0"), 0);
        assert_eq!(atoi(b" -2x"), -2);
        assert_eq!(atoi(b"abc"), 0);
        assert_eq!(atoi(b""), 0);
        assert_eq!(atoi(b"+07"), 7);
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
