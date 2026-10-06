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
//! one. So a signal that came under the registration before and that the loop
//! has not taken must not spend the new one. When a repeating watcher stops and
//! one-shot ones remain, `register` moves the `arrived` flag into the new
//! pair's `carried`, and the loop delivers that signal without spending the
//! registration, as natively it was caught before the `sigaction` with
//! `SA_RESETHAND` (review AR-50, part 2; cases
//! `uvloop/signal_reset_{usr1,urg}_after_repeating_stop`: a repeating watcher
//! W, a one-shot one O, and a `sync` dependent of W's promise, on the loop,
//! that computes while the signal comes, then stops W). Before, that signal
//! spent the registration, and the loop dropped the next one, which natively
//! a watcher started by O's promise gets. The pair's
//! registration waits for the handlers that are running (signal-hook's wait),
//! so by the move the flag holds every signal of the registration before;
//! without that wait, a late one would spend the new registration, as before.
//! A signal of the new registration whose handler has run the pair by the
//! move is moved with it and spends the registration ([`Reset::carry`],
//! review RF11-03). A window remains: a handler that sets `arrived` before
//! the move's swap of it and the pair's flag after the move's read of it
//! (one on another thread still in its pipe write when the move runs, for
//! example) has its signal moved without spending the registration, so the
//! next ignored signal is delivered once more, a stop signal stops the
//! process and is then delivered, and in the two-handler race (below) a
//! terminating one is delivered (review RF11-07).
//! At the first watcher's start a window remains: between `listen`'s clearing
//! of `arrived` (RSIOB-03) and the end of `register`, while `default` is still
//! set, an ignored signal's default action does nothing and the handler sets
//! the flag, which spends the new registration (natively the signal came
//! before `sigaction` and was discarded, review RF10-02: one delivery either
//! way). The other way round, a signal that came under a spent
//! registration and that the loop has not taken yet is not the next
//! registration's: `register` (and `unregister`) clears `arrived` when it
//! takes back a spent registration's pair (review RF10-01). Without that, a repeating watcher that
//! joins before the loop's next look would get the signal, and the one-shot one
//! with it, where natively the kernel discarded it. The flag of the
//! registration's first signal was taken when the loop spent it, so a flag set
//! since is a later signal's. The take-back waits for the handlers that are
//! running (signal-hook's wait), so every handler of the spent registration has
//! set the flag by the clear, and a flag set after it is the new registration's
//! signal; without that wait, a handler still running would have its signal
//! delivered, as before the clear.
//!
//! Two handlers of one signal can run at the same time on two threads (the
//! kernel blocks a signal only on the thread whose handler runs it), and both
//! can pass a reset pair's check before either sets its flag. Natively the
//! kernel resets the disposition when it delivers the first signal, in the
//! same step, and the second one takes the default action (review AR-50, part
//! 2). Within signal-hook's safe API and without descriptors of our own, the
//! check and the set cannot be one step: the safe actions are stores, which
//! leave the same state whether one handler ran or two, and an action of our
//! own (an atomic swap) needs `low_level::register`, which is unsafe (a pipe
//! per registration, whose bytes would count the handlers, would cost two
//! descriptors that native does not open, review RF11-05). What the loop can
//! see is the second handler's flag, set after the loop spent the
//! registration: at its next take, or at the registration's take-back
//! (`unregister`, when the watcher it delivered to was the last one; a
//! repeating watcher's start). For a signal that ends the process, the loop
//! then takes the default action itself ([`later_default`]). For such a
//! signal, that flag needs a handler that passed the check (the race), or one
//! that saw the flag set and ends the process itself: either way the process
//! ends, with the signal's status, as natively. The spending signal came
//! after the registration (one from before is carried; at the first watcher's
//! start, `default` being set, a terminating one ended the process in its
//! handler), so its handler ran the pair, and a later handler that passed the
//! check ran at the same time as that one. The loop takes the action when it
//! next looks at the flag, not when the second signal comes. Cases left,
//! recorded in docs/sched.md: when both handlers set `arrived` before the
//! loop takes it, the loop sees one signal, and the second one, natively the
//! default action, is lost; and a stop signal that the loop finds after the
//! registration is spent is dropped, since its handler may have stopped the
//! process already (RSIOB-11's state, after the process continued), so in the
//! race the process does not stop (nor at a take-back's window, review
//! RF11-04).
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

/// The stop signals of Lean's table (SIGTSTP, SIGTTIN, SIGTTOU): their
/// default action stops the process.
const STOP_SIGNALS: [i32; 3] = [20, 21, 22];

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

/// The default action of a later signal of a spent one-shot registration
/// that the loop finds, at its take or at the registration's take-back
/// (review AR-50, part 2): natively the kernel took it under `SIG_DFL`.
/// Its handler may have passed the reset pair's check, if it ran at the same
/// time as the first signal's handler, on another thread, before that one
/// set its flag; the module comment explains why the loop then takes the
/// action that the handler did not. A signal that ends the process ends it
/// here (signal-hook's `emulate_default_handler`: `SIG_DFL`, the signal
/// unblocked and raised again, so the status is the signal's and the kernel
/// dumps a core where the signal's default makes one; SIGIO's exit with
/// status 157, as its conditional defaults, RSIOB-06). A signal whose
/// default is to ignore it gets nothing (the loop drops it). A stop signal
/// gets nothing either: its handler, if it saw the flag set, has stopped
/// the process or is about to, and the loop cannot tell, so a second stop
/// would be one too many.
fn later_default(signum: i32) {
    if signum == SIGIO {
        signal_hook::low_level::exit(128 + SIGIO);
    }
    if !STOP_SIGNALS.contains(&signum) {
        let _ = signal_hook::low_level::emulate_default_handler(signum);
    }
}

/// The reset pair of one one-shot registration: its flag and its two
/// actions, whether the loop has taken a signal under it, and whether a
/// signal of the registration before it is still to deliver.
struct Reset {
    /// The pair's flag (its two actions hold their own references;
    /// [`Reset::carry`] and the tests read it here).
    flag: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
    /// The loop has taken a signal under this registration
    /// ([`Hooked::take`]), or `register`'s move took one with the signal
    /// from before ([`Reset::carry`]): natively the kernel has restored
    /// `SIG_DFL`, so the loop drops a later signal (review AR-50). Only the
    /// loop and the registration write it, under the `reset` lock; handlers
    /// never read it.
    spent: bool,
    /// A signal came under the registration that this one replaced, while
    /// watchers listened, and the loop had not taken it
    /// ([`Hooked::register`] with `carry`): the loop delivers it, but it
    /// does not spend this registration, as natively a signal caught
    /// before `sigaction` with `SA_RESETHAND` (review AR-50, part 2). Under
    /// the `reset` lock, as `spent`.
    carried: bool,
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
            carried: false,
        })
    }

    /// `register`'s move of a signal from the registration before:
    /// `arrived` into `carried`. The pair's flag is read first: a handler
    /// of this registration sets `arrived` before the pair's flag, so a
    /// flag set by then means that the move also takes that handler's
    /// signal, this registration's first, and the registration is spent
    /// (review RF11-03). A flag read after the move could be a handler's
    /// whose `arrived` came after the move: the loop's take must deliver
    /// that signal as the registration's first, and a spent registration
    /// would give it the default action instead. The order leaves a window
    /// (review RF11-07): a handler that sets `arrived` before the swap and
    /// the pair's flag after the read (one on another thread still in its
    /// pipe write when the move runs, for example) has its signal moved
    /// without spending the registration, so the next ignored signal is
    /// delivered once more, a stop signal stops the process and is then
    /// delivered, and in the two-handler race a terminating one is
    /// delivered.
    fn carry(&mut self, arrived: &AtomicBool) {
        self.spent = self.flag.load(Ordering::SeqCst);
        self.carried = arrived.swap(false, Ordering::SeqCst);
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
    /// taken back ([`Hooked::take_back`]), a pair on a fresh flag exactly
    /// for a one-shot handler, then the default's flag cleared. `carry` is
    /// `uv__signal_stop`'s re-registration, when a repeating watcher stops
    /// and one-shot ones are left: a signal that came under the registration
    /// it replaces and that the loop has not taken moves from `arrived` to
    /// the new pair's `carried` (review AR-50, part 2; [`Reset::carry`]).
    /// The pair's registration waits for the handlers that are running
    /// (signal-hook's wait), so every handler without the new pair has set
    /// `arrived` by then, and a flag set later is the new registration's
    /// signal.
    fn register(&self, signum: i32, oneshot: bool, carry: bool) {
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        self.take_back(signum, &mut r);
        if oneshot {
            *r = Reset::new(signum);
            if let (true, Some(new)) = (carry, r.as_mut()) {
                new.carry(&self.arrived);
            }
        }
        self.default.store(false, Ordering::SeqCst);
    }

    /// libuv's `uv__signal_unregister_handler`: the default action, then the
    /// reset pair taken back.
    fn unregister(&self, signum: i32) {
        self.default.store(true, Ordering::SeqCst);
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        self.take_back(signum, &mut r);
    }

    /// Takes back the current one-shot registration's reset pair, if any,
    /// with the `reset` lock held. A spent registration's signals that the
    /// loop has not taken came after its first one, so natively they took
    /// the default action: the take-back clears `arrived` (review RF10-01),
    /// and a signal that ends the process ends it ([`later_default`],
    /// review AR-50, part 2: a handler that saw the pair's flag set has
    /// done so or is about to). A signal carried from the registration
    /// before is put back into `arrived`, for the loop to deliver. The pair
    /// is taken back before the clear, so a stop signal that comes between
    /// the two at a repeating watcher's start is dropped, where natively it
    /// stops the process or is delivered (review RF11-04, docs/sched.md).
    fn take_back(&self, signum: i32, r: &mut Option<Reset>) {
        let Some(old) = r.take() else {
            return;
        };
        let (spent, carried) = (old.spent, old.carried);
        old.remove();
        if spent && self.arrived.swap(false, Ordering::SeqCst) {
            later_default(signum);
        }
        if carried {
            self.arrived.store(true, Ordering::SeqCst);
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
    /// A signal that ends the process and that the loop finds here has
    /// passed the pair's check, or is about to take its default action in
    /// its handler: the loop takes that action ([`later_default`], review
    /// AR-50, part 2). A signal carried from the registration before is
    /// delivered, also when a later signal of a spent registration comes
    /// with it (review RF11-03), and does not spend this one. The `reset`
    /// lock is held across the swap, so a registration comes wholly before
    /// or after it.
    fn take(&self, signum: i32) -> bool {
        let mut r = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        let new = self.arrived.swap(false, Ordering::SeqCst);
        let Some(reset) = r.as_mut() else {
            return new;
        };
        let carried = std::mem::take(&mut reset.carried);
        if !new {
            return carried;
        }
        if reset.spent {
            later_default(signum);
            return carried;
        }
        reset.spent = true;
        true
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
        h.register(signum, oneshot, false);
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
            x.unregister(signum);
        } else if !oneshot && x.oneshot.load(Ordering::SeqCst) == left {
            // `uv__signal_stop`: only one-shot watchers remain, so the
            // handler is registered again with `SA_RESETHAND`; a signal
            // that came before is not the new registration's
            x.register(signum, true, true);
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
        .filter(|(s, x)| x.take(*s))
        .map(|(s, _)| *s)
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::{handlers, native_signum, reset_pair, Action, Hooked, Reset, SIGIO};
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
        h.register(SIGURG, true, false);
        let r1 = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(!Arc::ptr_eq(&r1, &h.default));
        h.register(SIGURG, false, false);
        assert!(reset_flag(&h).is_none());
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(!r1.load(Ordering::SeqCst), "the first pair was taken back");
        h.register(SIGURG, true, true);
        let r2 = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(!Arc::ptr_eq(&r2, &r1) && !Arc::ptr_eq(&r2, &h.default));
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(r2.load(Ordering::SeqCst));
        assert!(!r1.load(Ordering::SeqCst));
        h.unregister(SIGURG);
        assert!(reset_flag(&h).is_none());
        assert!(takes_default(&h));
        h.register(SIGURG, true, false);
        let r3 = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(!Arc::ptr_eq(&r3, &r1) && !Arc::ptr_eq(&r3, &r2));
        h.unregister(SIGURG);
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
        h.register(SIGURG, true, false);
        assert!(!takes_default(&h));
        let old = reset_flag(&h).expect("a one-shot registration has a reset pair");
        // a repeating watcher joins the one-shot ones
        h.register(SIGURG, false, false);
        // the late handler's reset, done inside the take-back's wait
        old.store(true, Ordering::SeqCst);
        assert!(!takes_default(&h), "the repeating watcher listens");
        signal_hook::low_level::raise(SIGURG).expect("raise");
        assert!(!takes_default(&h), "no reset with a repeating watcher");
        // the repeating watcher stops: only one-shot ones remain
        h.register(SIGURG, true, true);
        assert!(!takes_default(&h));
        signal_hook::low_level::raise(SIGURG).expect("raise");
        let new = reset_flag(&h).expect("a one-shot registration has a reset pair");
        assert!(new.load(Ordering::SeqCst));
        assert!(takes_default(&h));
        // taken back: a signal sets the flag no more
        h.unregister(SIGURG);
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
        h.register(SIGURG, true, false);
        assert!(!h.take(SIGURG), "no signal came");
        arrive(&h);
        assert!(h.take(SIGURG), "A gets the first signal");
        // B starts in a `sync` dependent of A's promise: no registration
        arrive(&h);
        assert!(!h.take(SIGURG), "B does not get the second signal");
        assert!(!h.take(SIGURG));
        // RF10-01: a third signal under the spent registration, and a
        // repeating watcher W2 joins before the loop takes it: natively the
        // kernel discarded it, so neither B nor W2 gets it; both get the
        // next one
        arrive(&h);
        h.register(SIGURG, false, false);
        assert!(
            !h.take(SIGURG),
            "the spent registration's signal is no one's"
        );
        arrive(&h);
        assert!(h.take(SIGURG), "W2 and B get the next signal");
        // a fresh one-shot registration's first signal, not yet taken when
        // a repeating watcher joins: kept (natively the handler caught it)
        h.unregister(SIGURG);
        h.register(SIGURG, true, false);
        arrive(&h);
        h.register(SIGURG, false, false);
        assert!(
            h.take(SIGURG),
            "an unspent registration's signal is delivered"
        );
        // a fresh one-shot registration; the loop takes the first signal
        // before its handler's reset pair runs
        h.unregister(SIGURG);
        h.register(SIGURG, true, false);
        h.arrived.store(true, Ordering::SeqCst);
        assert!(h.take(SIGURG), "the first signal");
        signal_hook::low_level::raise(SIGURG).expect("raise");
        arrive(&h);
        assert!(!h.take(SIGURG), "the second signal is dropped all the same");
        // a repeating watcher joins: every signal
        h.register(SIGURG, false, false);
        for _ in 0..2 {
            arrive(&h);
            assert!(h.take(SIGURG), "the repeating watcher gets every signal");
        }
        // it stops, one-shot watchers remain: one signal again
        h.register(SIGURG, true, true);
        arrive(&h);
        assert!(
            h.take(SIGURG),
            "a fresh registration takes its first signal"
        );
        arrive(&h);
        assert!(!h.take(SIGURG));
        // none listens: the flag is taken (no watcher to deliver to; the
        // next first watcher clears it, RSIOB-03)
        h.unregister(SIGURG);
        arrive(&h);
        assert!(h.take(SIGURG));
        // a new first watcher, one-shot
        h.register(SIGURG, true, false);
        arrive(&h);
        assert!(h.take(SIGURG), "a new registration takes its first signal");
        h.unregister(SIGURG);
    }

    /// AR-50, part 2: a signal that came under the registration that a
    /// one-shot one replaces, while watchers listened (a repeating watcher W
    /// and a one-shot one O; W stops), and that the loop has not taken, is
    /// delivered (to O), but does not spend the new registration: natively
    /// it was caught before `sigaction` with `SA_RESETHAND`. So B, started
    /// by O's promise, gets the registration's first signal, and a later one
    /// is dropped (case `uvloop/signal_reset_urg_after_repeating_stop`).
    /// Before, the loop counted it as the registration's own and dropped
    /// B's signal; with `later_default`, a terminating one would have ended
    /// the process. A carried signal and one of the registration's own
    /// taken together spend it; a signal of the registration whose handler
    /// ran the pair before the move is moved with the one from before and
    /// spends it, and a later signal's take delivers the moved ones all the
    /// same (RF11-03); a carried one that a repeating watcher's
    /// arrival finds still there is delivered; the first watcher's
    /// registration carries nothing (RF10-02, unchanged). And a later
    /// signal of a spent registration that the last watcher's stop finds is
    /// cleared (SIGURG: nothing else).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_signal_before_a_reregistration_does_not_spend_it() {
        let _s = SIGURG_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let h = hooked_alone();
        // W, repeating, and O, one-shot, listen; a signal the loop has not
        // taken; W stops
        h.register(SIGURG, false, false);
        arrive(&h);
        h.register(SIGURG, true, true);
        assert!(!h.arrived.load(Ordering::SeqCst), "carried, not arrived");
        assert!(h.take(SIGURG), "O gets the signal from before");
        arrive(&h);
        assert!(h.take(SIGURG), "B gets the registration's first signal");
        arrive(&h);
        assert!(!h.take(SIGURG), "a later one is dropped");
        h.unregister(SIGURG);
        // the signal from before and the registration's own, taken together
        h.register(SIGURG, false, false);
        arrive(&h);
        h.register(SIGURG, true, true);
        arrive(&h);
        assert!(h.take(SIGURG), "one delivery");
        arrive(&h);
        assert!(!h.take(SIGURG), "spent by its own signal");
        h.unregister(SIGURG);
        // RF11-03: the registration's own signal before the move (its
        // handler sets `arrived`, then the pair's flag), as `register` does
        // it with the `reset` lock held
        h.register(SIGURG, false, false);
        arrive(&h);
        {
            let mut r = h.reset.lock().unwrap_or_else(PoisonError::into_inner);
            let mut new = Reset::new(SIGURG).expect("a reset pair");
            arrive(&h);
            new.carry(&h.arrived);
            assert!(new.spent && new.carried, "moved, and spent");
            *r = Some(new);
        }
        arrive(&h);
        assert!(
            h.take(SIGURG),
            "the moved signals are delivered with a later one"
        );
        arrive(&h);
        assert!(!h.take(SIGURG), "a later one is dropped");
        h.unregister(SIGURG);
        // a repeating watcher joins before the loop has taken it
        h.register(SIGURG, false, false);
        arrive(&h);
        h.register(SIGURG, true, true);
        h.register(SIGURG, false, false);
        assert!(h.take(SIGURG), "the signal from before is delivered");
        h.unregister(SIGURG);
        // the first watcher, one-shot: a flag set before the end of its
        // registration (RF10-02's window) spends it
        h.arrived.store(true, Ordering::SeqCst);
        h.register(SIGURG, true, false);
        assert!(h.take(SIGURG));
        arrive(&h);
        assert!(!h.take(SIGURG));
        // a later signal that the last watcher's stop finds
        arrive(&h);
        h.unregister(SIGURG);
        assert!(!h.arrived.load(Ordering::SeqCst));
    }

    /// AR-50, part 2: two handlers of a one-shot registration's signal, on
    /// two threads, can both pass the reset pair's check before either sets
    /// its flag, where natively the second signal takes the default action
    /// (`SA_RESETHAND`). The loop finds the second one's `arrived` after it
    /// spent the registration, at its take or at the registration's
    /// take-back (a repeating watcher joins, the last watcher stops), and
    /// takes the default action: a terminating signal ends the process
    /// with its own status (SIGUSR2), SIGIO exits with 157 (RSIOB-06). A
    /// stop signal (SIGTSTP) is dropped and does not stop the process: its
    /// handler may have stopped it already. Each run is a child process (the
    /// test runs itself again), where `arrived` is set without the pair's
    /// check, as when a handler passed it.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_later_signal_of_a_spent_registration_takes_its_default_action() {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "LEAN_RUNTIME_TEST_LATER_SIGNAL";
        const SIGUSR2: i32 = 12;
        const SIGTSTP: i32 = 20;
        if let Ok(v) = std::env::var(CHILD) {
            let (signum, at) = v.split_once(' ').expect("SIGNUM WHERE");
            let signum: i32 = signum.parse().expect("a signal number");
            let h = hooked_alone();
            h.register(signum, true, false);
            // the first signal's handler, then the loop's take
            h.arrived.store(true, Ordering::SeqCst);
            assert!(h.take(signum));
            // the second one's handler passed the pair's check
            h.arrived.store(true, Ordering::SeqCst);
            let delivered = match at {
                "take" => h.take(signum),
                "join" => {
                    h.register(signum, false, false);
                    h.take(signum)
                }
                "stop" => {
                    h.unregister(signum);
                    h.arrived.load(Ordering::SeqCst)
                }
                _ => unreachable!("{at}"),
            };
            println!("survived, delivered: {delivered}");
            return;
        }
        let run = |signum: i32, at: &str| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sched::uv_signals::tests::\
                     a_later_signal_of_a_spent_registration_takes_its_default_action",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, format!("{signum} {at}"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            // a stopped child would never end: give up after 60 s
            let t0 = std::time::Instant::now();
            let st = loop {
                if let Some(st) = child.try_wait().unwrap() {
                    break st;
                }
                if t0.elapsed() > std::time::Duration::from_secs(60) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{signum} at {at}: the child did not end (stopped?)");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            let mut out = String::new();
            use std::io::Read;
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut out)
                .unwrap();
            (st, out)
        };
        for at in ["take", "join", "stop"] {
            let (st, out) = run(SIGUSR2, at);
            assert_eq!(
                st.signal(),
                Some(SIGUSR2),
                "SIGUSR2 at {at}: {st:?} {out:?}"
            );
            let (st, out) = run(SIGIO, at);
            assert_eq!(st.code(), Some(157), "SIGIO at {at}: {st:?} {out:?}");
            let (st, out) = run(SIGTSTP, at);
            assert!(st.success(), "SIGTSTP at {at}: {st:?} {out:?}");
            assert!(
                out.contains("survived, delivered: false"),
                "SIGTSTP at {at}: {out:?}"
            );
        }
    }
}
