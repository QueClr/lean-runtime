//! The process-wide part of `Std.Internal.UV.Signal`'s delivery, shared by
//! both schedulers' `uv`: the single-thread one (`src/sched/uv.rs`, feature
//! `sched`) and threads mode's (`src/sched/mt/uv.rs`, feature `threads`).
//! A build has one of them. Here: each signal's handlers (signal-hook's safe
//! API only, review RSIOB-05), the loop's signal pipe, the counts of the
//! watchers that listen, and libuv's (re)registration of a signal's handler.
//! The list of listening watchers is each scheduler's own: per thread on one
//! thread (each scheduler's loop delivers to its own watchers), one for the
//! process in threads mode (its one loop thread delivers to every watcher,
//! as libuv's one loop does).
//!
//! - Each signal's handlers, installed at its first watcher and never taken
//!   back (signal-hook cannot restore a disposition), run in the order they
//!   were registered: `flag::register` sets the signal's `arrived` flag,
//!   `low_level::pipe::register_raw` writes a byte into the loop's signal
//!   pipe, then the conditional default action, then, while every listener
//!   is one-shot, a second `flag::register` that sets the default's flag.
//! - The pipe is native's own (`io::startup`, made at startup as libuv makes
//!   it) when the glue opened native's startup descriptors, else one made at
//!   the first watcher; either way its descriptors live in a static for the
//!   life of the process, never closed, `dup2`'d over or reused, and its
//!   write end is never unregistered (signal-hook's handlers write to it by
//!   number: a wrong descriptor is no undefined behaviour, but corrupts
//!   whatever the number names).
//! - The loop watches the read end; when it is readable, [`arrived`] drains
//!   the pipe until `EAGAIN`, then takes each signal's `arrived` flag, and
//!   the loop delivers the signals that came to its watchers of them.
//!
//! The conditional default action runs the signal's default action while no
//! watcher listens (natively libuv restores `SIG_DFL`); for SIGIO, which
//! signal-hook's table lacks, an exit with status 157 (128 + SIGIO), as a
//! shell reports the signal's death (RSIOB-06). While every listener of a
//! signal is one-shot, the second flag makes a second signal take the
//! default action before the loop has delivered the first, as libuv's
//! `SA_RESETHAND` (RSIOB-02); it is registered and unregistered
//! (`low_level::unregister`) as `uv__signal_start` and `uv__signal_stop`
//! re-register libuv's handler.
//!
//! Locks: `HOOKED` and a signal's `reset`, each held only around plain data
//! and signal-hook's registration calls, never across a scheduler's lock or
//! translator code; `HOOKED` before `reset`. In threads mode both are taken
//! with `sched::uv`'s loop lock held (every extern holds it).

use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// `EINVAL`.
pub(crate) const EINVAL: i32 = 22;

/// `SIGIO`, whose default action signal-hook's table lacks (review
/// RSIOB-06).
pub(crate) const SIGIO: i32 = 29;

/// `lean_uv_signal_mk`'s table (Lean's `Std.Internal.IO.Async.Signal`
/// numbers, Linux's): the signal, or 0 for one it does not know, which
/// `next` refuses (`UV_EINVAL`).
pub(crate) fn native_signum(n: i32) -> i32 {
    match n {
        1 | 2 | 3 | 5 | 6 | 10 | 12 | 14 | 15 | 17 | 18 | 20 | 21 | 22 | 23 | 24 | 25 | 26 | 27
        | 28 | 29 | 31 => n,
        _ => 0,
    }
}

/// Per signal, process-wide: the number of watchers listening (and of the
/// one-shot ones among them), the flag of the conditional default action
/// (set while none listens, or after a signal while only one-shot ones do),
/// the reset action that sets it, and the `arrived` flag.
struct Hooked {
    listening: AtomicUsize,
    oneshot: AtomicUsize,
    default: Arc<AtomicBool>,
    reset: Mutex<Option<signal_hook::SigId>>,
    arrived: Arc<AtomicBool>,
}

static HOOKED: Mutex<Vec<(i32, Arc<Hooked>)>> = Mutex::new(Vec::new());

/// The loop's signal pipe (read end, write end), for the life of the
/// process.
enum Pipe {
    #[cfg_attr(not(feature = "io"), allow(dead_code))]
    Startup(BorrowedFd<'static>, BorrowedFd<'static>),
    Own(OwnedFd, OwnedFd),
}

impl Pipe {
    fn read(&'static self) -> BorrowedFd<'static> {
        match self {
            Pipe::Startup(r, _) => *r,
            Pipe::Own(r, _) => r.as_fd(),
        }
    }
    fn write(&'static self) -> BorrowedFd<'static> {
        match self {
            Pipe::Startup(_, w) => *w,
            Pipe::Own(_, w) => w.as_fd(),
        }
    }
}

/// Never dropped (a static): see the module comment.
static PIPE: OnceLock<Pipe> = OnceLock::new();

/// The pipe, made at the first watcher. A failure to make it (`EMFILE`) is
/// that watcher's error, and the next watcher tries again (review
/// RSIOB-16).
fn pipe() -> Result<&'static Pipe, i32> {
    if let Some(p) = PIPE.get() {
        return Ok(p);
    }
    #[cfg(feature = "io")]
    if let Some((r, w)) = crate::io::startup::claim_signal_pipe() {
        return Ok(PIPE.get_or_init(|| Pipe::Startup(r, w)));
    }
    use rustix::pipe::{pipe_with, PipeFlags};
    let (r, w) =
        pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).map_err(|e| e.raw_os_error())?;
    // one thread makes it at a time: the watchers' registration holds
    // `HOOKED`'s lock (`hooked`)
    Ok(PIPE.get_or_init(|| Pipe::Own(r, w)))
}

/// The pipe's read end, once a watcher has made the pipe.
#[cfg_attr(not(feature = "threads"), allow(dead_code))]
pub(crate) fn pipe_read() -> Option<BorrowedFd<'static>> {
    PIPE.get().map(|p| p.read())
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// The order of watchers' creation (`Signal::new`).
pub(crate) fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn io_err(e: std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EINVAL)
}

fn hooked(signum: i32) -> Result<Arc<Hooked>, i32> {
    let mut h = HOOKED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, x)) = h.iter().find(|(s, _)| *s == signum) {
        return Ok(x.clone());
    }
    let pipe = pipe()?;
    let arrived = Arc::new(AtomicBool::new(false));
    let default = Arc::new(AtomicBool::new(true));
    // The flag before the byte: a reader woken by the byte finds it set.
    signal_hook::flag::register(signum, arrived.clone()).map_err(io_err)?;
    signal_hook::low_level::pipe::register_raw(signum, pipe.write().as_raw_fd()).map_err(io_err)?;
    if signum == SIGIO {
        signal_hook::flag::register_conditional_shutdown(signum, 128 + SIGIO, default.clone())
            .map_err(io_err)?;
    } else {
        signal_hook::flag::register_conditional_default(signum, default.clone()).map_err(io_err)?;
    }
    let x = Arc::new(Hooked {
        listening: AtomicUsize::new(0),
        oneshot: AtomicUsize::new(0),
        default,
        reset: Mutex::new(None),
        arrived,
    });
    h.push((signum, x.clone()));
    Ok(x)
}

impl Hooked {
    /// libuv's (re)registration of the signal's handler, with
    /// `SA_RESETHAND` when `oneshot`: the default's flag cleared, and the
    /// reset action present exactly for a one-shot handler.
    fn register(&self, signum: i32, oneshot: bool) {
        self.default.store(false, Ordering::SeqCst);
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        if oneshot {
            if r.is_none() {
                *r = signal_hook::flag::register(signum, self.default.clone()).ok();
            }
        } else if let Some(id) = r.take() {
            signal_hook::low_level::unregister(id);
        }
    }

    /// libuv's `uv__signal_unregister_handler`: the default action.
    fn unregister(&self) {
        self.default.store(true, Ordering::SeqCst);
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = r.take() {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// A watcher of `signum` starts listening (`uv_signal_start`,
/// `uv_signal_start_oneshot`): the signal's handlers installed at its first
/// watcher, its counts, and libuv's (re)registration. Returns the pipe's
/// read end, which the loop watches while a watcher listens. The caller
/// then adds the watcher to its list.
pub(crate) fn listen(signum: i32, oneshot: bool) -> Result<BorrowedFd<'static>, i32> {
    let h = hooked(signum)?;
    let read = pipe()?.read();
    let n = h.listening.load(Ordering::SeqCst);
    let os = h.oneshot.load(Ordering::SeqCst);
    if n == 0 {
        // A signal that came while no watcher listened is no watcher's
        // (natively no handler of libuv's caught it): review RSIOB-03.
        h.arrived.store(false, Ordering::SeqCst);
    }
    // `uv__signal_start`: (re)register when no watcher listens, or when
    // only one-shot ones do and a repeating one comes.
    if n == 0 || (!oneshot && os == n) {
        h.register(signum, oneshot);
    }
    h.listening.fetch_add(1, Ordering::SeqCst);
    if oneshot {
        h.oneshot.fetch_add(1, Ordering::SeqCst);
    }
    Ok(read)
}

/// A watcher of `signum` that listened stops (`uv_signal_stop`), once the
/// caller has taken it off its list: its counts, and the default action
/// when none is left, or the one-shot registration when only one-shot ones
/// are left.
pub(crate) fn unlisten(signum: i32, oneshot: bool) {
    let h = HOOKED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, x)) = h.iter().find(|(s, _)| *s == signum) {
        let left = x.listening.fetch_sub(1, Ordering::SeqCst) - 1;
        if oneshot {
            x.oneshot.fetch_sub(1, Ordering::SeqCst);
        }
        if left == 0 {
            x.unregister();
        } else if !oneshot && x.oneshot.load(Ordering::SeqCst) == left {
            // `uv__signal_stop`: only one-shot watchers remain, so the
            // handler is registered again with `SA_RESETHAND`
            x.register(signum, true);
        }
    }
}

/// The loop's call when the pipe is readable: drain the pipe until
/// `EAGAIN`, then take each signal's `arrived` flag. The signals that came,
/// in signal-number order (occurrences of one signal between two calls are
/// one delivery, where libuv makes one per occurrence).
pub(crate) fn arrived() -> Vec<i32> {
    let Ok(p) = pipe() else {
        return Vec::new();
    };
    let mut buf = [0u8; 64];
    loop {
        match rustix::io::read(p.read(), &mut buf) {
            Ok(n) if n > 0 => continue,
            Err(rustix::io::Errno::INTR) => continue,
            _ => break,
        }
    }
    let h = HOOKED.lock().unwrap_or_else(PoisonError::into_inner);
    let mut v: Vec<i32> = h
        .iter()
        .filter(|(_, x)| x.arrived.swap(false, Ordering::SeqCst))
        .map(|(s, _)| *s)
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::native_signum;

    #[test]
    fn lean_signal_numbers() {
        assert_eq!(native_signum(10), 10);
        assert_eq!(native_signum(31), 31);
        for n in [0, 4, 7, 8, 9, 11, 13, 16, 19, 30, 32, -1, 99] {
            assert_eq!(native_signum(n), 0, "{n}");
        }
    }
}
