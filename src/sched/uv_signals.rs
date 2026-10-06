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
//!   were registered ([`handlers`]): the conditional default action first,
//!   then `flag::register` sets the signal's `arrived` flag, then
//!   `low_level::pipe::register_raw` writes a byte into the loop's signal
//!   pipe. While every listener is one-shot, a reset pair follows
//!   ([`reset_pair`]), on a flag of that registration's own: a second
//!   conditional default action, then a `flag::register` that sets its flag.
//! - The pipe is native's own (`io::startup`, made at startup as libuv makes
//!   it) when the glue opened native's startup descriptors, else one made at
//!   the first watcher; either way its descriptors live in a static for the
//!   life of the process, never closed, `dup2`'d over or reused, and its
//!   write end is never unregistered (signal-hook's handlers write to it by
//!   number: a wrong descriptor is no undefined behaviour, but corrupts
//!   whatever the number names).
//! - The loop watches the read end; when it is readable, [`arrived`] drains
//!   the pipe until `EAGAIN`, then takes each signal's `arrived` flag, and
//!   the loop delivers the signals that came to its watchers of them. Under
//!   a one-shot registration, the loop delivers only the first signal it
//!   takes ([`Hooked::take`], below).
//!
//! The first conditional default action, on the signal's `default` flag,
//! runs the signal's default action while no watcher listens (natively
//! libuv restores `SIG_DFL`); for SIGIO, which signal-hook's table lacks, an
//! exit with status 157 (128 + SIGIO), as a shell reports the signal's
//! death (RSIOB-06). The check of `default`, the flag the loop can change,
//! runs before the byte, so the loop's delivery and a `stop` of the last
//! watcher that follows cannot turn a caught signal into the default
//! action, as natively the kernel decides the disposition at delivery
//! (review AR-49: the handler read `default` after its byte, and threads
//! mode's twin of `uvloop/signal_stop_in_sync_dependent` ended with status
//! 138 now and then). The reset pair's check runs after the byte, but reads
//! only its registration's own flag, which only handlers write. While every
//! listener of a signal is one-shot, the reset pair makes a second signal
//! take the default action before the loop has delivered the first, as
//! libuv's `SA_RESETHAND` (RSIOB-02): the first signal passes its check,
//! then sets its flag. Each one-shot registration gets a pair on a fresh
//! flag, registered and unregistered (`low_level::unregister`, the check
//! first) as `uv__signal_start` and `uv__signal_stop` re-register libuv's
//! handler. So `default` means only "no watcher listens", and a handler
//! still running from an older registration sets only that registration's
//! flag, which nothing reads any more. Before, the reset set `default`
//! itself: when a repeating watcher joined one-shot ones, `register`
//! cleared `default`, then unregistered the reset, which waited for a
//! handler between its check and its reset; that handler then set
//! `default`, and the next signal took the default action although the
//! repeating watcher listened (AR-49). The fresh flag also makes the fix
//! independent of that wait, signal-hook's for the handlers that are
//! running (an internal detail of signal-hook-registry 1.4.8). At the first
//! one-shot registration only, RSIOB-02 in the narrow window between the
//! pair's registration and the clearing of `default` relies on that wait:
//! a handler that started before the registration ends inside it, while
//! `default` is still set (the main race and the late reset do not).
//!
//! The reset pair's default action comes after the flag and the byte. For a
//! signal whose default is to ignore it (SIGCHLD, SIGCONT, SIGURG, SIGWINCH) it
//! does nothing; for a stop signal it stops the process, and the flag and the
//! byte are there when the process continues. So the loop takes only one signal
//! under each one-shot registration ([`Hooked::take`]) and drops the later
//! ones, where natively the kernel's `SIG_DFL` discards them or only stops the
//! process (review AR-50). The state where it matters is RSIOB-11's: a one-shot
//! watcher started by a `sync` dependent of another one-shot watcher's promise
//! finds that one still listening, so libuv registers nothing again and the new
//! watcher never gets the signal (case
//! `uvloop/signal_reset_urg_in_sync_dependent`). Before, it got the second
//! signal: the reset pair's check had passed it on, and the flag and the byte
//! were already there. The loop marks the registration spent (`Reset::spent`,
//! which no handler reads) when it takes a signal, not when the kernel delivers
//! one. So a signal that came before a registration, but that the loop takes
//! after it, spends the new registration, as natively a signal that came just
//! after it would. That needs a signal between the loop's last look and the
//! registration, and a one-shot watcher started by a `sync` dependent of that
//! delivery. The registrations: when a repeating watcher stops and one-shot
//! ones remain; and at the first watcher's start, between `listen`'s clearing
//! of `arrived` (RSIOB-03) and the end of `register`, while `default` is still
//! set, where an ignored signal's default action does nothing and the handler
//! sets the flag (natively the signal came before `sigaction` and was
//! discarded, review RF10-02). Natively the signal's time decides between the
//! two outcomes too. The other way round, a signal that came under a spent
//! registration and that the loop has not taken yet is not the next
//! registration's: `register` clears `arrived` when it takes back a spent
//! registration's pair (review RF10-01). Without that, a repeating watcher that
//! joins before the loop's next look would get the signal, and the one-shot one
//! with it, where natively the kernel discarded it. The flag of the
//! registration's first signal was taken when the loop spent it, so a flag set
//! since is a later signal's. The take-back waits for the handlers that are
//! running (signal-hook's wait), so every handler of the spent registration has
//! set the flag by the clear, and a flag set after it is the new registration's
//! signal; without that wait, a handler still running would have its signal
//! delivered, as before the clear.
//!
//! Locks: `HOOKED` and a signal's `reset`, each held only around plain data
//! and signal-hook's registration calls, never across a scheduler's lock or
//! translator code; `HOOKED` before `reset`. In threads mode both are taken
//! with `sched::uv`'s loop lock held (every extern holds it).

use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
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
/// one-shot ones among them), the flag of the first conditional default
/// action (set while none listens), the reset pair of the current one-shot
/// registration, and the `arrived` flag.
struct Hooked {
    listening: AtomicUsize,
    oneshot: AtomicUsize,
    default: Arc<AtomicBool>,
    reset: Mutex<Option<Reset>>,
    arrived: Arc<AtomicBool>,
}

/// One action of a signal's handler, as signal-hook registers it.
enum Action {
    /// The default action while the flag is set:
    /// `flag::register_conditional_default`, or, for SIGIO,
    /// `flag::register_conditional_shutdown` with status 157 (RSIOB-06).
    Default(Arc<AtomicBool>),
    /// `flag::register`: sets the flag.
    Set(Arc<AtomicBool>),
    /// `low_level::pipe::register_raw`: a byte into the loop's signal pipe.
    Pipe(RawFd),
}

/// A signal's handlers, installed at its first watcher, in the order they
/// run (signal-hook runs a signal's actions in registration order): the
/// default action's check first, so a handler has decided before its byte
/// wakes the loop (review AR-49); the `arrived` flag before the byte, so a
/// reader woken by the byte finds it set.
fn handlers(default: &Arc<AtomicBool>, arrived: &Arc<AtomicBool>, pipe: RawFd) -> [Action; 3] {
    [
        Action::Default(default.clone()),
        Action::Set(arrived.clone()),
        Action::Pipe(pipe),
    ]
}

/// The reset of one one-shot registration (libuv's `SA_RESETHAND`), on that
/// registration's own flag: the check, then the set, so the first signal is
/// caught and a second one takes the default action (RSIOB-02).
fn reset_pair(flag: &Arc<AtomicBool>) -> [Action; 2] {
    [Action::Default(flag.clone()), Action::Set(flag.clone())]
}

/// Registers `actions` for `signum`, in their order. On a failure, the ones
/// registered so far are taken back, but never a pipe's: signal-hook would
/// close its descriptor (the module comment).
fn install(signum: i32, actions: &[Action]) -> Result<Vec<signal_hook::SigId>, i32> {
    use signal_hook::{flag, low_level};
    let mut ids = Vec::with_capacity(actions.len());
    let mut undo = Vec::new();
    for a in actions {
        let r = match a {
            Action::Default(f) if signum == SIGIO => {
                flag::register_conditional_shutdown(signum, 128 + SIGIO, f.clone())
            }
            Action::Default(f) => flag::register_conditional_default(signum, f.clone()),
            Action::Set(f) => flag::register(signum, f.clone()),
            Action::Pipe(fd) => low_level::pipe::register_raw(signum, *fd),
        };
        match r {
            Ok(id) => {
                ids.push(id);
                if !matches!(a, Action::Pipe(_)) {
                    undo.push(id);
                }
            }
            Err(e) => {
                for id in undo {
                    low_level::unregister(id);
                }
                return Err(io_err(e));
            }
        }
    }
    Ok(ids)
}

/// The reset pair of one one-shot registration: its flag and its two
/// actions, and whether the loop has taken a signal under it.
struct Reset {
    /// The pair's flag (its two actions hold their own references; the
    /// tests read it here).
    #[cfg_attr(not(test), allow(dead_code))]
    flag: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
    /// The loop has taken a signal under this registration
    /// ([`Hooked::take`]): natively the kernel has restored `SIG_DFL`, so
    /// the loop drops a later signal (review AR-50). Only the loop and the
    /// registration write it, under the `reset` lock; handlers never read
    /// it.
    spent: bool,
}

impl Reset {
    /// A pair on a fresh flag, or none if signal-hook refuses it (the
    /// handler then has no reset).
    fn new(signum: i32) -> Option<Reset> {
        let flag = Arc::new(AtomicBool::new(false));
        let ids = install(signum, &reset_pair(&flag)).ok()?;
        Some(Reset {
            flag,
            ids,
            spent: false,
        })
    }

    /// Takes the pair back, the check first: a handler that starts between
    /// the two only sets the flag, which nothing reads any more.
    fn remove(self) {
        for id in self.ids {
            signal_hook::low_level::unregister(id);
        }
    }
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
    let write = pipe.write().as_raw_fd();
    install(signum, &handlers(&default, &arrived, write))?;
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
    /// `SA_RESETHAND` when `oneshot`: the last registration's reset pair
    /// taken back, a pair on a fresh flag exactly for a one-shot handler,
    /// then the default's flag cleared. A spent registration's signals that
    /// the loop has not taken came after its first one, so natively they
    /// took the default action: the take-back clears `arrived` (review
    /// RF10-01).
    fn register(&self, signum: i32, oneshot: bool) {
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = r.take() {
            let spent = old.spent;
            old.remove();
            if spent {
                self.arrived.store(false, Ordering::SeqCst);
            }
        }
        if oneshot {
            *r = Reset::new(signum);
        }
        self.default.store(false, Ordering::SeqCst);
    }

    /// libuv's `uv__signal_unregister_handler`: the default action, then the
    /// reset pair taken back.
    fn unregister(&self) {
        self.default.store(true, Ordering::SeqCst);
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = r.take() {
            old.remove();
        }
    }

    /// The loop takes the signal's `arrived` flag: whether a signal came
    /// that the loop delivers. Under a one-shot registration (`SA_RESETHAND`)
    /// the first signal the loop takes spends the registration, and the loop
    /// drops a later one: natively the kernel restored `SIG_DFL` when it
    /// delivered the first, and the later one took the default action
    /// (review AR-50). For a signal that ends or stops the process, the
    /// reset pair's check takes that action in the handler. For one whose
    /// default is to ignore it (SIGCHLD, SIGCONT, SIGURG, SIGWINCH), the
    /// pair's action does nothing, and the handler has already set the flag
    /// and written the byte: the drop is that signal's default action. The
    /// state: a one-shot watcher started in a `sync` dependent of another's
    /// promise (RSIOB-11), and, for a stop signal, the process continued.
    /// The `reset` lock is held across the swap, so a registration comes
    /// wholly before or after it.
    fn take(&self) -> bool {
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.arrived.swap(false, Ordering::SeqCst) {
            return false;
        }
        match r.as_mut() {
            Some(reset) if reset.spent => false,
            Some(reset) => {
                reset.spent = true;
                true
            }
            None => true,
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
/// `EAGAIN`, then take each signal's `arrived` flag ([`Hooked::take`]:
/// under a one-shot registration, only the first signal). The signals that
/// came, in signal-number order (occurrences of one signal between two
/// calls are one delivery, where libuv makes one per occurrence).
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
        .filter(|(_, x)| x.take())
        .map(|(s, _)| *s)
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::{handlers, native_signum, reset_pair, Action, Hooked};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    #[test]
    fn lean_signal_numbers() {
        assert_eq!(native_signum(10), 10);
        assert_eq!(native_signum(31), 31);
        for n in [0, 4, 7, 8, 9, 11, 13, 16, 19, 30, 32, -1, 99] {
            assert_eq!(native_signum(n), 0, "{n}");
        }
    }

    /// AR-49: signal-hook runs a signal's actions in registration order, and
    /// `hooked` and `Reset::new` register these lists. The default action's
    /// check comes before the `arrived` flag and the pipe's byte, so a
    /// handler has decided before it wakes the loop; a reset pair checks its
    /// flag before it sets it, so the first signal is caught (RSIOB-02).
    #[test]
    fn the_default_is_checked_before_the_loop_wakes() {
        let default = Arc::new(AtomicBool::new(true));
        let arrived = Arc::new(AtomicBool::new(false));
        let h = handlers(&default, &arrived, 7);
        assert!(matches!(&h[0], Action::Default(f) if Arc::ptr_eq(f, &default)));
        assert!(matches!(&h[1], Action::Set(f) if Arc::ptr_eq(f, &arrived)));
        assert!(matches!(h[2], Action::Pipe(7)));
        let flag = Arc::new(AtomicBool::new(false));
        let r = reset_pair(&flag);
        assert!(matches!(&r[0], Action::Default(f) if Arc::ptr_eq(f, &flag)));
        assert!(matches!(&r[1], Action::Set(f) if Arc::ptr_eq(f, &flag)));
    }

    /// SIGURG: no other test uses it, and its default action is to ignore
    /// it, so these tests register its actions in the test process.
    const SIGURG: i32 = 23;

    /// The tests that register SIGURG's actions, one at a time: a signal
    /// runs every registered pair.
    static SIGURG_TESTS: Mutex<()> = Mutex::new(());

    /// A signal's state with no handlers of `hooked` (no pipe), for
    /// `Hooked::register` and `Hooked::unregister` alone.
    fn hooked_alone() -> Hooked {
        Hooked {
            listening: AtomicUsize::new(0),
            oneshot: AtomicUsize::new(0),
            default: Arc::new(AtomicBool::new(true)),
            reset: Mutex::new(None),
            arrived: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The flag of the current registration's reset pair.
    fn reset_flag(h: &Hooked) -> Option<Arc<AtomicBool>> {
        let r = h.reset.lock().unwrap_or_else(PoisonError::into_inner);
        r.as_ref().map(|r| r.flag.clone())
    }

    /// Whether the next signal takes the default action: no watcher
    /// listens, or the current one-shot registration has had its signal.
    fn takes_default(h: &Hooked) -> bool {
        h.default.load(Ordering::SeqCst) || reset_flag(h).is_some_and(|f| f.load(Ordering::SeqCst))
    }

    /// AR-49: each one-shot registration gets a reset pair on a fresh flag,
    /// never `default`; a repeating registration and the unregistration
    /// take it back, so a signal sets only the current flag. The sequence
    /// is libuv's: the first watcher one-shot, a repeating one joins, it
    /// stops (only one-shot ones remain), the last one stops, a new first
    /// one-shot watcher.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn each_oneshot_registration_has_a_fresh_reset_flag() {
        let _s = SIGURG_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let h = hooked_alone();
        h.register(SIGURG, true);
        let r1 = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(!Arc::ptr_eq(&r1, &h.default));
        h.register(SIGURG, false);
        assert!(reset_flag(&h).is_none());
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(!r1.load(Ordering::SeqCst), "the first pair was taken back");
        h.register(SIGURG, true);
        let r2 = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(!Arc::ptr_eq(&r2, &r1) && !Arc::ptr_eq(&r2, &h.default));
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(r2.load(Ordering::SeqCst));
        assert!(!r1.load(Ordering::SeqCst));
        h.unregister();
        assert!(reset_flag(&h).is_none());
        assert!(takes_default(&h));
        h.register(SIGURG, true);
        let r3 = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(!Arc::ptr_eq(&r3, &r1) && !Arc::ptr_eq(&r3, &r2));
        h.unregister();
    }

    /// AR-49's second race: one-shot watchers listen, and a handler sits
    /// between its reset pair's check and its reset when a repeating
    /// watcher joins them (`register(false)`). The pair's take-back waits
    /// for that handler (signal-hook's wait for running handlers), so its
    /// reset lands during the registration. It sets only the old
    /// registration's flag, and the repeating watcher listens. Before, the
    /// reset set `default` itself after `register` had cleared it, and the
    /// next signal took the default action although the repeating watcher
    /// listened. Then, with only one-shot watchers again, a real signal:
    /// the new pair's flag takes it, so a second one would take the
    /// default action (RSIOB-02).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_late_reset_leaves_a_new_watcher_listening() {
        let _s = SIGURG_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let h = hooked_alone();
        h.register(SIGURG, true);
        assert!(!takes_default(&h));
        let old = reset_flag(&h).expect("a one-shot registration has a reset pair");
        // a repeating watcher joins the one-shot ones
        h.register(SIGURG, false);
        // the late handler's reset, done inside the take-back's wait
        old.store(true, Ordering::SeqCst);
        assert!(!takes_default(&h), "the repeating watcher listens");
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(!takes_default(&h), "no reset with a repeating watcher");
        // the repeating watcher stops: only one-shot ones remain
        h.register(SIGURG, true);
        assert!(!takes_default(&h));
        signal_hook::low_level::raise(SIGURG).expect("raise");
        let new = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(new.load(Ordering::SeqCst));
        assert!(takes_default(&h));
        // taken back: a signal sets the flag no more
        h.unregister();
        new.store(false, Ordering::SeqCst);
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(!new.load(Ordering::SeqCst));
    }

    /// The handler's part, as `hooked`'s actions and a reset pair run it:
    /// the `arrived` flag (the pipe's byte only wakes the loop), then a real
    /// SIGURG, which runs the current registration's reset pair.
    fn arrive(h: &Hooked) {
        h.arrived.store(true, Ordering::SeqCst);
        signal_hook::low_level::raise(SIGURG).expect("raise");
    }

    /// AR-50: under a one-shot registration the loop delivers only the
    /// first signal it takes, and drops the later ones, as natively the
    /// kernel's `SIG_DFL` discards SIGURG after `SA_RESETHAND`; the reset
    /// pair's default action does nothing for SIGURG. First RSIOB-11's
    /// state: B, started by a `sync` dependent of A's promise, finds A
    /// listening, so nothing registers again, and B does not get the
    /// second signal (before, `take` delivered it). The rule does not
    /// depend on the time of the pair's reset: the loop may take a signal
    /// before its handler has run the pair (threads mode). A signal under
    /// the spent registration that the loop has not taken when a repeating
    /// watcher joins is dropped too (RF10-01), but an unspent
    /// registration's is kept. Then libuv's re-registrations: a repeating
    /// watcher joins (no reset: every signal), it stops (a fresh one-shot
    /// registration), the last watcher stops (no registration), a new first
    /// one-shot watcher.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_spent_oneshot_registration_drops_a_later_signal() {
        let _s = SIGURG_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let h = hooked_alone();
        // A, the first watcher, one-shot
        h.register(SIGURG, true);
        assert!(!h.take(), "no signal came");
        arrive(&h);
        assert!(h.take(), "A gets the first signal");
        // B starts in a `sync` dependent of A's promise: no registration
        arrive(&h);
        assert!(!h.take(), "B does not get the second signal");
        assert!(!h.take());
        // RF10-01: a third signal under the spent registration, and a
        // repeating watcher W2 joins before the loop takes it: natively the
        // kernel discarded it, so neither B nor W2 gets it; both get the
        // next one
        arrive(&h);
        h.register(SIGURG, false);
        assert!(!h.take(), "the spent registration's signal is no one's");
        arrive(&h);
        assert!(h.take(), "W2 and B get the next signal");
        // a fresh one-shot registration's first signal, not yet taken when
        // a repeating watcher joins: kept (natively the handler caught it)
        h.unregister();
        h.register(SIGURG, true);
        arrive(&h);
        h.register(SIGURG, false);
        assert!(h.take(), "an unspent registration's signal is delivered");
        // a fresh one-shot registration; the loop takes the first signal
        // before its handler's reset pair runs
        h.unregister();
        h.register(SIGURG, true);
        h.arrived.store(true, Ordering::SeqCst);
        assert!(h.take(), "the first signal");
        signal_hook::low_level::raise(SIGURG).expect("raise");
        arrive(&h);
        assert!(!h.take(), "the second signal is dropped all the same");
        // a repeating watcher joins: every signal
        h.register(SIGURG, false);
        for _ in 0..2 {
            arrive(&h);
            assert!(h.take(), "the repeating watcher gets every signal");
        }
        // it stops, one-shot watchers remain: one signal again
        h.register(SIGURG, true);
        arrive(&h);
        assert!(h.take(), "a fresh registration takes its first signal");
        arrive(&h);
        assert!(!h.take());
        // none listens: the flag is taken (no watcher to deliver to; the
        // next first watcher clears it, RSIOB-03)
        h.unregister();
        arrive(&h);
        assert!(h.take());
        // a new first watcher, one-shot
        h.register(SIGURG, true);
        arrive(&h);
        assert!(h.take(), "a new registration takes its first signal");
        h.unregister();
    }
}
