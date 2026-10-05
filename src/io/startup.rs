//! The descriptors native Lean has open before `main` (A821),
//! `IO.initializing`'s flag ([`initializing`]), and `main` on a thread of
//! its own with Lean's stack ([`run_main`], `lean_run_main`).
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
//!    which `sched::uv`'s signal watchers are woken, as libuv's are (or a
//!    translator's own, [`claim_signal_pipe`]);
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
//! - get native's startup descriptors opened before Rust's runtime starts:
//!   - **with the feature `startup-fds`** (lean2rr), the crate's own ELF
//!     constructor opens them (`.init_array.00101`, a native quirk written
//!     with `unsafe`: `UNSAFE.md`, `docs/native-quirks.md`, "The startup
//!     descriptors in a constructor"). The glue writes no constructor and
//!     no `unsafe`; it calls `ensure_native_descriptors` at the start of
//!     `main`, which does nothing more when the constructor opened them
//!     (and else opens them where they land, without native's numbers:
//!     the constructor acts only in the program's own executable); that
//!     call (or `mark_end_initialization`'s) also keeps the constructor
//!     linked, which a `#[used]` static alone does not;
//!   - **without it**, a glue that wants native's descriptors writes its
//!     own ELF constructor (`#[link_section = ".init_array"]`, as
//!     `tests/io_cases.rs` does), which calls [`open_native_descriptors`],
//!     and on `Err` [`end_startup`]; in plain `.init_array` or with a
//!     priority above 101, so that with the feature `proc-title` the crate's
//!     own constructor (`.init_array.00100`) runs first, as `lean_setup_args`
//!     runs before libuv's descriptors open (AR-20; with a priority of 100 or
//!     less it may not, and under `ulimit -n 12` the title's checks then find
//!     too few free descriptors). A glue must not do both: with the feature,
//!     its own constructor's call only returns the first outcome. A glue
//!     that writes no constructor (leanrs, its DV19) opens them after Rust's
//!     start or not at all, and a standard descriptor closed at startup then
//!     reads as Rust's `/dev/null`;
//! - call [`mark_end_initialization`] once the module initializers have run,
//!   before `main`, as the generated `main` calls
//!   `lean_io_mark_end_initialization` (also when an initializer failed);
//! - run `main` as `lean_run_main` does, on a thread of its own with Lean's
//!   stack size ([`run_main`], with a scheduler), and install Lean's
//!   stack-overflow report on each thread that runs Lean code, after Rust's
//!   runtime has started, never from an ELF constructor
//!   (`sched::install_stack_overflow_handler`, feature `stack-overflow`).
//!
//! **The constructors' order** (AR-20). The linkers put the `.init_array.N`
//! sections first, by increasing priority N, then the plain `.init_array`
//! ones in link order, and glibc calls the executable's entries in that
//! order: the toolchain's (priorities 90 and 99), `proc-title`'s (100),
//! `startup-fds`'s (101), then the program's own (a priority above 101, or
//! none: a glue's constructor of its own). So the arguments' memory is kept
//! before libuv's descriptors open, as natively, and both run before Rust's
//! runtime replaces closed standard descriptors (in `main`'s `lang_start`).
//! Neither crate constructor uses the global allocator (AR-36): an allocator
//! made in a constructor (mimalloc's first arena) would be set up before the
//! program configures it.
//!
//! The crate does the rest of the list only where it can: `SIGPIPE`'s
//! disposition belongs to the entry, only the glue knows when the
//! initializers end, and the stack-overflow report must be installed after
//! Rust's runtime start.
//!
//! So on a host with io_uring, descriptors 3 to 10 are taken, and a standard
//! descriptor closed at startup is taken by the first of them: reading a
//! closed stdin or writing a closed stdout then fails with `EINVAL`, and the
//! point where opening a file fails with `EMFILE` is native's. The
//! descriptors open in an ELF constructor, before Rust's runtime puts
//! `/dev/null` in the place of closed standard descriptors: afterwards such a
//! `/dev/null` cannot be told from one the program was given (a constructor
//! needs `#[link_section]`, which `deny(unsafe_code)` refuses: hence the
//! feature, or the glue's own).
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
//! submitted to it, so it has no completion. In threads mode (feature
//! `threads`) the epoll descriptor stays as opened: `sched::uv`'s loop
//! thread, made at the first use of the loop, waits with `poll(2)` on the
//! eventfd and the signal pipe instead.
//!
//! Opening them allocates nothing ([`open_native_descriptors`] runs in an
//! ELF constructor with `startup-fds`; AR-36): the rings are kept in an
//! array, `/proc/version_signature` is read into a stack buffer, and a
//! failure's line is written from one.
//!
//! Source: lean2rr's `runtime/leanrt/src/rt.rs` (`reserve_libuv_descriptors`,
//! `kernel_version`), rewritten over rustix's safe API and the io-uring
//! crate's. lean2rr's version
//! makes the control ring even when `uv__use_io_uring` says no, which libuv
//! 1.48.0 does not (`UV_USE_IO_URING=0` gives 6 descriptors natively).
//! [`run_main`] is lean2rr's `rt::run_main`'s thread (audit item 4.2), and
//! the feature `startup-fds` its startup constructor (item 4.3).

use io_uring::IoUring;
use rustix::event::{epoll, eventfd, EventfdFlags};
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
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
/// `main` runs; so does each translator's glue. (With `startup-fds` it also
/// keeps the crate's startup constructor linked: every glue calls it.)
pub fn mark_end_initialization() {
    #[cfg(feature = "startup-fds")]
    super::startup_fds::keep_constructor();
    INITIALIZING.store(false, Ordering::Relaxed)
}

/// Why the event loop could not be made, so the program does not reach
/// `main`, with the `errno` of the call that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupFailure {
    /// `uv_loop_init` failed (the epoll descriptor, the signal pipe or the
    /// eventfd could not be made). Natively `uv_default_loop` returns NULL
    /// and `event_loop_init`'s `uv_async_init` on it crashes (SIGSEGV, 139;
    /// LB-30).
    LoopInit(i32),
    /// The signal lock pipe could not be made or written. Natively libuv
    /// calls `abort()` (`uv__signal_global_reinit`; SIGABRT, 134; LB-31).
    SignalLock(i32),
}

impl StartupFailure {
    /// The `errno` of the call that failed.
    pub fn errno(self) -> i32 {
        match self {
            StartupFailure::LoopInit(e) | StartupFailure::SignalLock(e) => e,
        }
    }

    /// The message after `INTERNAL PANIC: `: `Failed to initialize event
    /// loop: ` and libuv's message for the error (`uv_strerror`), the shape
    /// of Lean's own `check_uv` messages in `event_loop_init`
    /// (`Failed to initialize event loop: too many open files` for
    /// `EMFILE`).
    pub fn message(self) -> String {
        let mut s = String::new();
        let _ = self.write_message(&mut s);
        s
    }

    /// [`StartupFailure::message`] into `w`, with no allocation of its own.
    fn write_message(self, w: &mut impl std::fmt::Write) -> std::fmt::Result {
        let code = super::error::crt_to_uv(self.errno());
        w.write_str("Failed to initialize event loop: ")?;
        match super::error::uv_strerror_named(code) {
            Some(m) => w.write_str(m),
            None => write!(w, "Unknown system error {code}"),
        }
    }
}

/// A line built on the stack ([`end_startup`] in an ELF constructor, which
/// must not allocate: AR-36). A longer line is cut, which no message reaches
/// (the longest is under 120 bytes).
struct StackLine {
    buf: [u8; 256],
    len: usize,
}

impl std::fmt::Write for StackLine {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// The descriptors, kept open for the life of the process, as libuv keeps
/// them.
struct Descriptors {
    #[cfg_attr(not(feature = "sched"), allow(dead_code))]
    epoll: OwnedFd,
    /// The polling ring and the control ring, each if the kernel gave it (an
    /// array, not a `Vec`: no allocation, AR-36).
    _rings: [Option<IoUring>; 2],
    _lock_pipe: (OwnedFd, OwnedFd),
    /// Kept open for the life of the process, never closed, `dup2`'d over or
    /// reused: signal handlers write to its write end by number (signal-hook's
    /// in `sched::uv`, or a translator's own; [`claim_signal_pipe`];
    /// docs/sched.md, "Std.Internal.UV").
    signal_pipe: (OwnedFd, OwnedFd),
    #[cfg_attr(not(any(feature = "net", feature = "threads")), allow(dead_code))]
    eventfd: OwnedFd,
}

static DESCRIPTORS: OnceLock<Result<Descriptors, StartupFailure>> = OnceLock::new();

/// Open native Lean's startup descriptors, once (later calls return the first
/// outcome). On `Err`, the event loop could not be made and the program
/// does not reach `main`: the translator ends the process with
/// [`end_startup`].
///
/// A glue's own ELF constructor calls it (without the feature
/// `startup-fds`). With `startup-fds` the crate's constructor has called it
/// already, and a glue calls [`ensure_native_descriptors`] instead.
/// It allocates nothing, unless `UV_USE_IO_URING` is set (read with
/// `std::env::var_os`; the crate's constructor reads it with `getenv`).
pub fn open_native_descriptors() -> Result<(), StartupFailure> {
    #[cfg(feature = "startup-fds")]
    super::startup_fds::keep_constructor();
    let env = std::env::var_os("UV_USE_IO_URING");
    open_native_descriptors_with(env.as_ref().map(|v| v.as_encoded_bytes()))
}

/// [`open_native_descriptors`] with `UV_USE_IO_URING`'s value given (`None`
/// when unset): the crate's constructor reads it without allocating.
pub(crate) fn open_native_descriptors_with(
    uv_use_io_uring: Option<&[u8]>,
) -> Result<(), StartupFailure> {
    match DESCRIPTORS.get_or_init(|| open_all(uv_use_io_uring)) {
        Ok(_) => Ok(()),
        Err(f) => Err(*f),
    }
}

/// Whether [`open_native_descriptors`] has run (whatever its outcome).
#[cfg(feature = "startup-fds")]
pub(crate) fn descriptors_opened() -> bool {
    DESCRIPTORS.get().is_some()
}

/// Native Lean's startup descriptors at the start of `main`, with the
/// feature `startup-fds`: the crate's ELF constructor opened them before
/// Rust's runtime started, and then this does nothing more. If it did not
/// (the crate in a shared library, a launch through the dynamic loader, no
/// `/proc`), this opens them now, where they land: it closes nothing, so a
/// standard descriptor closed at startup stays Rust's `/dev/null` and the
/// numbers are not native's (reviews RSH2-04, LS2-01); a failure ends the
/// process ([`end_startup`]). Call it at the start of `main`, before the
/// program opens a file, so that the numbers are as close as they can be.
/// It also keeps the constructor linked (a `#[used]` static alone does not
/// keep its object file in an rlib). (The constructor is a native quirk
/// written with `unsafe`: `UNSAFE.md`, `docs/native-quirks.md`, "The
/// startup descriptors in a constructor".)
#[cfg(feature = "startup-fds")]
pub use super::startup_fds::ensure_native_descriptors;

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

/// The read and write ends of the loop's signal pipe (the one libuv's
/// `uv__process_init` makes at startup), once [`open_native_descriptors`]
/// has opened them, for the one event loop that delivers
/// `Std.Internal.UV.Signal`'s signals, so that its watchers open no
/// descriptor of their own, as natively (review RSIOB-05). With the feature
/// `startup-fds` the crate's ELF constructor opened them before `main`: a
/// glue that wants the pipe enables the feature rather than write a
/// constructor of its own (the module comment, "The translator's glue
/// duties").
///
/// **Who may claim it.** `sched::uv`'s signal watchers (feature `sched`)
/// claim it at the first watcher. A translator that keeps its own scheduler
/// and signal watchers over this crate's `io` may claim it instead, with or
/// without `sched` (AR-17). The first caller gets it, as with
/// `claim_loop_epoll`; every later call, and every call before
/// [`open_native_descriptors`] or after it failed, gets `None`, and that
/// caller makes a pipe of its own (`sched::uv` does): so one loop drains
/// the pipe, as libuv's one loop does.
///
/// **The claimer's duty** (docs/sched.md, "Std.Internal.UV", the pipe's
/// duty). The descriptors belong to this module's static and stay open for
/// the life of the process. The claimer:
/// - never closes them, `dup2`s over them, or wraps their numbers in an
///   owning type (`OwnedFd`, `File`), whose drop would close them;
/// - uses them only as the signal pipe: signal handlers may write to the
///   write end by number at any time, so the numbers must never name
///   another file (such a write would corrupt it);
/// - keeps both ends non-blocking (they are made non-blocking and
///   close-on-exec, as libuv's), so a handler's write never blocks.
pub fn claim_signal_pipe() -> Option<(BorrowedFd<'static>, BorrowedFd<'static>)> {
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
/// loop through it, and so do `net`'s DNS helpers; in threads mode the
/// externs wake `sched::uv`'s loop thread through it (`uv_async_send`).
#[cfg(any(feature = "net", feature = "threads"))]
pub(crate) fn loop_eventfd() -> Option<BorrowedFd<'static>> {
    match DESCRIPTORS.get() {
        Some(Ok(d)) => Some(d.eventfd.as_fd()),
        _ => None,
    }
}

/// libuv's `uv_loop_init`, with `UV_USE_IO_URING`'s value given. Allocates
/// nothing (AR-36).
fn open_all(uv_use_io_uring: Option<&[u8]>) -> Result<Descriptors, StartupFailure> {
    let cloexec = epoll::CreateFlags::CLOEXEC;
    let loop_init = |e: rustix::io::Errno| StartupFailure::LoopInit(e.raw_os_error());
    let signal_lock = |e: rustix::io::Errno| StartupFailure::SignalLock(e.raw_os_error());
    let epoll = epoll::create(cloexec).map_err(loop_init)?;
    let mut rings = [None, None];
    if use_io_uring_with(uv_use_io_uring) {
        // `uv__iou_init` twice; a ring the kernel does not give is skipped.
        // The polling ring is watched for completions (`POLLIN`, data its
        // number); if that fails, libuv closes it.
        if let Some(ring) = ring(POLLING_ENTRIES, true) {
            let data = epoll::EventData::new_u64(ring.as_raw_fd() as u64);
            if epoll::add(&epoll, &ring, data, epoll::EventFlags::IN).is_ok() {
                rings[0] = Some(ring);
            }
        }
        rings[1] = ring(CONTROL_ENTRIES, false);
    }
    let lock_pipe = pipe_with(PipeFlags::CLOEXEC).map_err(signal_lock)?;
    // `uv__signal_unlock`
    rustix::io::write(&lock_pipe.1, &[42]).map_err(signal_lock)?;
    let signal_pipe = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).map_err(loop_init)?;
    let eventfd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).map_err(loop_init)?;
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

/// End the process when the event loop could not be made, before any module
/// code: `INTERNAL PANIC: Failed to initialize event loop: <libuv's
/// message>` on standard error, then exit status 1, or an abort under
/// `LEAN_ABORT_ON_PANIC`, as Lean's `check_uv` ends a failed step of the
/// same initialization (`lean_internal_panic`). Nothing is on standard
/// output yet.
///
/// Natively the loop's failure is a crash (LB-30: SIGSEGV, 139, the
/// unchecked `uv_default_loop()`; LB-31: SIGABRT, 134, libuv's `abort()`
/// when the signal lock pipe cannot be made), with the outcome depending on
/// how few descriptors are left; `docs/lean-bugs.md` (case
/// `io/startup_fd_exhausted`).
///
/// The line is built on the stack and written to descriptor 2 at once (the
/// unbuffered standard error, which no stream has used yet), so the call
/// allocates nothing unless `LEAN_ABORT_ON_PANIC` is set (read with
/// `std::env::var_os`; the crate's constructor reads it with `getenv`).
pub fn end_startup(failure: StartupFailure) -> ! {
    let abort = std::env::var_os("LEAN_ABORT_ON_PANIC");
    end_startup_with(failure, abort.as_ref().map(|v| v.as_encoded_bytes()))
}

/// [`end_startup`] with `LEAN_ABORT_ON_PANIC`'s value given (`None` when
/// unset). Allocates nothing (AR-36).
pub(crate) fn end_startup_with(failure: StartupFailure, abort_on_panic: Option<&[u8]>) -> ! {
    use crate::semantics::panic::{
        internal_panic_end, PanicEnd, PanicSettings, INTERNAL_PANIC_PREFIX, PANIC_EXIT_STATUS,
    };
    use std::fmt::Write;
    let mut line = StackLine {
        buf: [0; 256],
        len: 0,
    };
    let _ = line.write_str(INTERNAL_PANIC_PREFIX);
    let _ = failure.write_message(&mut line);
    let _ = line.write_str("\n");
    let mut rest = &line.buf[..line.len];
    while !rest.is_empty() {
        match rustix::io::write(rustix::stdio::stderr(), rest) {
            Ok(0) => break,
            Ok(n) => rest = &rest[n..],
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => break,
        }
    }
    let s = PanicSettings::from_env(abort_on_panic, None);
    match internal_panic_end(s) {
        PanicEnd::Abort => std::process::abort(),
        _ => std::process::exit(PANIC_EXIT_STATUS),
    }
}

/// [`end_startup`], under the name the glue was first written against (it
/// ended the process as native's crash then).
pub fn fail_as_native(failure: StartupFailure) -> ! {
    end_startup(failure)
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
    let env = std::env::var_os("UV_USE_IO_URING");
    use_io_uring_with(env.as_ref().map(|v| v.as_encoded_bytes()))
}

/// [`use_io_uring`] with `UV_USE_IO_URING`'s value given (`None` when
/// unset).
fn use_io_uring_with(uv_use_io_uring: Option<&[u8]>) -> bool {
    match uv_use_io_uring {
        Some(v) => atoi(v) != 0,
        None => kernel_version() >= 0x05_0A_BA,
    }
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
/// + c`, 0 when unknown. Allocates nothing (AR-36).
pub fn kernel_version() -> u32 {
    let mk = |(a, b, c): (u32, u32, u32)| {
        a.wrapping_mul(65536)
            .wrapping_add(b.wrapping_mul(256))
            .wrapping_add(c)
    };
    let mut buf = [0u8; 256];
    if let Some(sig) = slurp(c"/proc/version_signature", &mut buf) {
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

/// libuv 1.48.0's `uv__slurp(path, buf, len)`: one `read` of at most
/// `len - 1` bytes (retried on `EINTR`) from the file opened close-on-exec;
/// the bytes read, `None` when it cannot be opened or read.
fn slurp<'a>(path: &std::ffi::CStr, buf: &'a mut [u8]) -> Option<&'a [u8]> {
    use rustix::fs::{open, Mode, OFlags};
    let fd = open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).ok()?;
    let max = buf.len().saturating_sub(1);
    loop {
        match rustix::io::read(&fd, &mut buf[..max]) {
            Ok(n) => return Some(&buf[..n]),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// `main` on a thread of its own

/// Whether `main` runs on a thread of its own ([`run_main`]), set before its
/// body starts.
#[cfg(any(feature = "sched", feature = "threads"))]
static MAIN_ON_THREAD: AtomicBool = AtomicBool::new(false);

/// Whether [`run_main`] runs `main` on a thread of its own: false with
/// `LEAN_MAIN_USE_THREAD=0`, where it runs on the calling thread (the
/// initializers'), so that a glue that gives `main` fresh per-thread state
/// (lean2rr's standard-stream context) does so only on its own thread, as
/// natively a new thread starts with none. Read inside `main`'s body.
#[cfg(any(feature = "sched", feature = "threads"))]
pub fn main_on_thread() -> bool {
    MAIN_ON_THREAD.load(Ordering::Relaxed)
}

/// `lean_run_main`'s choice (`run_with_thread_stack`): the calling thread
/// when `LEAN_MAIN_USE_THREAD` is exactly `0` (`strcmp(v, "0") == 0`), else
/// a thread of its own.
#[cfg(any(feature = "sched", feature = "threads"))]
fn main_uses_thread(lean_main_use_thread: Option<&[u8]>) -> bool {
    lean_main_use_thread != Some(b"0")
}

/// Run the program's `main` as Lean's `lean_run_main` does
/// (`src/runtime/thread.cpp`): on a new thread with a stack of `stack_size`
/// bytes, which it waits for; or on the calling thread when
/// `LEAN_MAIN_USE_THREAD` is `0`. The caller passes Lean's size, read when
/// `main` starts (after the module initializers, as `lean_run_main` reads
/// `LEAN_STACK_SIZE_KB`): [`crate::sched::thread_stack_size`] (1 GiB on
/// 64-bit targets, or `LEAN_STACK_SIZE_KB` rounded down to 4 KiB plus
/// 128 KiB), or a size of its own.
///
/// - The thread has no name of its own, as native's (`lthread` names none),
///   so it keeps the process's (`/proc/thread-self/comm`).
/// - When it cannot be made, the process aborts with libc++'s report of
///   Lean's uncaught `failed to create thread: <strerror>`
///   ([`crate::sched::thread_create_failed`]; status 134).
/// - `body`'s value is returned. A Rust panic (a bug of the runtime or of
///   the translator: Lean code does not panic in Rust) comes back as `Err`,
///   with its payload, only when it unwinds out of `body` through frames
///   that can unwind, on `main`'s thread or on the calling one, after the
///   panic hook printed it; the glue decides how the process ends then. A
///   panic in frames that cannot unwind (a translator's generated
///   `extern "C"` code, the crate's own `extern "C"` points) aborts the
///   process where it happens, before `run_main` sees it: status 134
///   (`SIGABRT`), with the bytes buffered in the standard streams not
///   written. On a thread of its own, the panic hook's header names the
///   thread `'<unnamed>'`, as the thread has no name (as native's
///   `lthread`); with `LEAN_MAIN_USE_THREAD=0` it names the calling thread.
/// - [`main_on_thread`] says which thread `body` runs on.
///
/// The caller's duties:
/// - `body` installs Lean's stack-overflow report first
///   (`sched::install_stack_overflow_handler()` with the feature
///   `stack-overflow`: natively each Lean thread installs it at its start),
///   as the crate installs nothing here;
/// - `body` holds the whole of `main`'s life with tasks: `sched::start` (or
///   `start_with`, `start_lazy`), the program's `main`, then
///   `sched::finish`, all inside `body`, on its thread. The single-thread
///   scheduler's state is that thread's own (thread-locals): a `start`
///   before `run_main` would start a scheduler on the calling thread, which
///   `main` never sees (review RSH2-03);
/// - the module initializers ran before, on the calling thread, with its
///   own stack, so that a deep initializer overflows as natively.
#[cfg(any(feature = "sched", feature = "threads"))]
pub fn run_main<T, F>(stack_size: usize, body: F) -> std::thread::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let env = std::env::var_os("LEAN_MAIN_USE_THREAD");
    if !main_uses_thread(env.as_ref().map(|v| v.as_encoded_bytes())) {
        MAIN_ON_THREAD.store(false, Ordering::Relaxed);
        return std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    }
    MAIN_ON_THREAD.store(true, Ordering::Relaxed);
    match std::thread::Builder::new()
        .stack_size(stack_size)
        .spawn(body)
    {
        Ok(t) => t.join(),
        // natively `lthread` throws `lean::exception("failed to create
        // thread: " << strerror(err))`, which nothing catches: libc++
        // reports it and aborts
        Err(e) => crate::sched::thread_create_failed(&e),
    }
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

    /// AR-17: the startup signal pipe under `io` alone. Before
    /// `open_native_descriptors` a claim gets `None` (and claims nothing);
    /// after it the first claim gets the two ends of one pipe, read and
    /// write, both non-blocking and close-on-exec, and every later claim
    /// gets `None`. In a child process of its own, so that the descriptors
    /// open nowhere else and no other test claims first. (With
    /// `startup-fds` the crate's constructor opened them before `main`.)
    #[test]
    #[cfg_attr(miri, ignore)] // Miri runs no process or file system call
    fn signal_pipe_claimed_once() {
        use rustix::fs::{fcntl_getfl, fstat, OFlags};
        use rustix::io::{fcntl_getfd, FdFlags};
        const CHILD: &str = "LEAN_RUNTIME_TEST_SIGNAL_PIPE";
        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "io::startup::tests::signal_pipe_claimed_once",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("1 passed"),
                "{out:?}"
            );
            return;
        }
        if !cfg!(feature = "startup-fds") {
            assert!(claim_signal_pipe().is_none());
        }
        open_native_descriptors().unwrap();
        let (r, w) = claim_signal_pipe().unwrap();
        assert!(claim_signal_pipe().is_none());
        let (sr, sw) = (fstat(r).unwrap(), fstat(w).unwrap());
        assert_eq!(
            rustix::fs::FileType::from_raw_mode(sr.st_mode),
            rustix::fs::FileType::Fifo
        );
        assert_eq!((sr.st_dev, sr.st_ino), (sw.st_dev, sw.st_ino));
        let (fr, fw) = (fcntl_getfl(r).unwrap(), fcntl_getfl(w).unwrap());
        assert_eq!(fr & OFlags::ACCMODE, OFlags::RDONLY);
        assert_eq!(fw & OFlags::ACCMODE, OFlags::WRONLY);
        for (fl, fd) in [(fr, r), (fw, w)] {
            assert!(fl.contains(OFlags::NONBLOCK));
            assert!(fcntl_getfd(fd).unwrap().contains(FdFlags::CLOEXEC));
        }
        // a byte written to the write end comes out of the read end, and
        // then the empty pipe's read does not block
        assert_eq!(rustix::io::write(w, &[7]), Ok(1));
        let mut b = [0u8; 2];
        assert_eq!(rustix::io::read(r, &mut b), Ok(1));
        assert_eq!(b[0], 7);
        assert_eq!(rustix::io::read(r, &mut b), Err(rustix::io::Errno::AGAIN));
    }

    /// `lean_run_main`'s choice: only exactly `0` keeps the calling thread.
    #[test]
    #[cfg(any(feature = "sched", feature = "threads"))]
    fn main_uses_a_thread_unless_zero() {
        assert!(main_uses_thread(None));
        assert!(!main_uses_thread(Some(b"0")));
        for v in [&b""[..], b"1", b"00", b" 0", b"0 ", b"false"] {
            assert!(main_uses_thread(Some(v)), "{v:?}");
        }
    }

    /// `run_main` runs the body on a new thread with the stack asked for,
    /// which keeps the process's name (no name of its own, as `lthread`),
    /// and returns its value; a panic of the body comes back as `Err`.
    #[test]
    #[cfg(any(feature = "sched", feature = "threads"))]
    #[cfg_attr(miri, ignore)] // reads `/proc`
    fn run_main_runs_on_a_thread_of_its_own() {
        if std::env::var_os("LEAN_MAIN_USE_THREAD").is_some_and(|v| v == "0") {
            eprintln!("LEAN_MAIN_USE_THREAD=0 in the environment: nothing checked");
            return;
        }
        let comm = || std::fs::read_to_string("/proc/thread-self/comm").unwrap();
        // libtest names its test threads; a new thread inherits the name of
        // the thread that makes it, as natively `main`'s inherits the
        // process's
        let outer = (std::thread::current().id(), comm());
        let size = 3 << 20;
        let (id, name, on_thread, local) = run_main(size, move || {
            let local = 0u8;
            (
                std::thread::current().id(),
                comm(),
                main_on_thread(),
                std::ptr::addr_of!(local).addr(),
            )
        })
        .unwrap();
        assert_ne!(id, outer.0);
        assert_eq!(name, outer.1);
        assert!(on_thread);
        // the body ran on a stack of its own: not on this thread's
        let here = 0u8;
        let distance = std::ptr::addr_of!(here).addr().abs_diff(local);
        assert!(distance > 64 << 10, "{distance}");
        let r = run_main(size, || -> u8 { panic!("a runtime bug") });
        assert!(r.is_err());
    }

    /// A thread `run_main` cannot make (a stack larger than the address
    /// space) ends the process with libc++'s report of Lean's uncaught
    /// exception and an abort, as native's `lthread`. In a child process.
    #[test]
    #[cfg(any(feature = "sched", feature = "threads"))]
    #[cfg_attr(miri, ignore)] // Miri runs no process
    fn run_main_without_a_thread_aborts_as_native() {
        const CHILD: &str = "LEAN_RUNTIME_TEST_RUN_MAIN_FAILS";
        if std::env::var_os(CHILD).is_none() {
            use std::os::unix::process::ExitStatusExt;
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "io::startup::tests::run_main_without_a_thread_aborts_as_native",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .env_remove("LEAN_MAIN_USE_THREAD")
                .output()
                .unwrap();
            assert_eq!(out.status.signal(), Some(6), "{out:?}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains(
                    "libc++abi: terminating due to uncaught exception of type lean::exception: \
                     failed to create thread: Resource temporarily unavailable\n"
                ),
                "{err}"
            );
            return;
        }
        let _ = run_main(1 << 62, || ());
        unreachable!("the thread was made");
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

    /// The failure's line, built without an allocation, is the message
    /// `StartupFailure::message` gives, with Lean's prefix; a code libuv
    /// does not name gets its `Unknown system error` text.
    #[test]
    fn failure_lines() {
        use std::fmt::Write;
        for (f, text) in [
            (
                StartupFailure::SignalLock(24),
                "Failed to initialize event loop: too many open files",
            ),
            (
                StartupFailure::LoopInit(12),
                "Failed to initialize event loop: not enough memory",
            ),
            (
                StartupFailure::LoopInit(4000),
                "Failed to initialize event loop: Unknown system error -4000",
            ),
        ] {
            assert_eq!(f.message(), text);
            let mut line = StackLine {
                buf: [0; 256],
                len: 0,
            };
            f.write_message(&mut line).unwrap();
            assert_eq!(&line.buf[..line.len], text.as_bytes());
        }
        // a longer line is cut at the buffer's end
        let mut line = StackLine {
            buf: [0; 256],
            len: 250,
        };
        line.write_str("0123456789").unwrap();
        assert_eq!((line.len, &line.buf[250..]), (256, &b"012345"[..]));
    }

    /// libuv's `uv__slurp`: at most `len - 1` bytes, the file's start.
    #[test]
    #[cfg_attr(miri, ignore)] // reads `/proc`
    fn slurp_reads_one_buffer_less_one() {
        let mut buf = [0u8; 8];
        let got = slurp(c"/proc/self/stat", &mut buf).unwrap();
        assert_eq!(got.len(), 7);
        let whole = std::fs::read("/proc/self/stat").unwrap();
        assert_eq!(got, &whole[..7]);
        assert_eq!(slurp(c"/nonexistent/file", &mut buf), None);
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
