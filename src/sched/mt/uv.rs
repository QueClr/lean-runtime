//! `Std.Internal.UV`'s event loop, timers and signals in threads mode
//! (feature `threads`; docs/threads.md, 0.5): Lean 4.34.0's
//! `src/runtime/uv/event_loop.cpp`, `timer.cpp` and `signal.cpp` over
//! libuv 1.48, with the single-thread `sched::uv`'s names and shapes
//! (`LoopPromise`, `Timer`, `Signal`, `loop_configure`, `loop_alive`), so a
//! translator's glue compiles against either mode with a `cfg` (leanrs's
//! constraints for T1, point 1). `sched` re-exports this module as
//! `sched::uv`. A failure is a libuv error code (a negative `errno`), which
//! the glue turns into Lean's error with `io::IoError::decode_uv_error(code,
//! None)`.
//!
//! **Which thread runs the loop.** A thread of its own, the *loop thread*,
//! as natively: `initialize_libuv` starts an `lthread` that runs
//! `event_loop_run_loop` for the life of the process (`libuv.cpp` 19-27).
//! Here it is made at the first use of the loop (any extern below), not at
//! startup, so a program that never uses `Std.Internal.UV` has no such
//! thread. It has Lean's thread stack size (the task manager's, as
//! `lthread`'s), installs Lean's stack-overflow report at its entry (feature
//! `stack-overflow`, as `lthread::imp::_main`'s `stack_guard`), calls the
//! glue's `thread_start` once (at its start, or, made before
//! `sched::start`, once a glue appears; review RT2-05), and never ends (no
//! `thread_end`). A Rust panic on it aborts the process, as on every thread
//! the crate makes.
//!
//! **The loop lock** ([`LoopLock`]: native `event_loop_t`'s recursive
//! `mutex` and `n_waiters`, `event_loop.cpp` 66-102). The loop thread holds
//! it during each iteration: it waits in `poll(2)` for the earliest timer,
//! the signal pipe (while a watcher listens) and the loop's async eventfd;
//! then it delivers the signals that came and runs the timers due
//! (`uv_run(UV_RUN_ONCE)`: `uv__io_poll`, then `uv__run_timers`), and lets
//! the lock go. Every extern takes the lock first. When another thread
//! holds it, the extern counts itself a waiter and writes the eventfd
//! (`uv_async_send`): the loop thread ends its iteration, so the signals
//! that came and the timers due are handled before the extern acts, and it
//! waits while a waiter is left before its next iteration (85-91). The lock
//! is recursive per thread: a callback's `sync` dependent that calls an
//! extern on the loop thread goes on at once, with no iteration.
//!
//! **Who resolves what.**
//! - The loop thread resolves a timer's promise when the timer fires
//!   (`handle_timer_event`), and a signal watcher's when its signal came
//!   (`handle_signal_event`), through [`LoopPromise::resolve`], with the loop
//!   lock held. The glue's `resolve` is `sched::resolve`, so the promise's
//!   `sync` dependents run there, on the loop thread, its other dependents
//!   are queued for the pool, and its waiters wake.
//! - An extern resolves nothing itself. It makes a handle's promise
//!   (`new_promise`, `lean_io_promise_new`) and drops the handle's reference
//!   to one (`stop`, `cancel`, a repeating handle's `next`), on the calling
//!   thread with the loop lock held, as natively `lean_dec(m_promise)` runs
//!   under `event_loop_lock`. When that was the last reference to an
//!   unresolved promise, the glue resolves it with `none`
//!   (`deactivate_promise`) there, and its `sync` dependents run there.
//! - A one-shot timer or watcher is finished before its promise resolves
//!   (LB-20 not copied), and a failed `next` holds no extra reference (LB-19
//!   not copied), as in the single-thread `sched::uv` (docs/sched.md).
//!
//! **Lock order.** The loop lock first. Under it: a handle's state lock (a
//! timer's, a signal watcher's, with `net` a socket's) and the loop's `data`
//! lock, each held only for plain data and never across a promise's
//! resolution or drop (a handle's state lock may be held while `data` is
//! taken, never the other way); and, through translator code (a promise's
//! `new`, `resolve` and drop, and the `sync` dependents they run), the
//! scheduler's lock, the stream locks, `CWD_LOCK` and `uv_signals`' locks.
//! Nothing holding one of those takes the loop lock, except translator code
//! that the loop lock's holder runs itself (recursive, the same thread). So a
//! `sync` dependent on the loop thread that waits for a task which calls an
//! extern on another thread waits for good, as natively (the loop thread
//! holds the lock meanwhile). A glue must not call an extern while it holds a
//! stream's guard (`io::Handle::file()`), nor drop a socket's last handle
//! then: the drop is Lean's finalizer, which takes the loop lock.
//!
//! With `net`, the leaf locks (held only around plain data, taking no other
//! lock): the async queue's (`posted`, taken without the loop lock too), and
//! `net`'s registry of open sockets, its table of lookups in progress and
//! its spare descriptor (`mode_mt`'s `SOCKETS` and `PENDING`, `tcp`'s
//! `EMFILE_FD`). A socket's state lock is taken only under the loop lock;
//! the translator code that runs under it is the glue's `SendData::get` and
//! `RecvBuf::target`, which only hand out bytes: they call nothing of the
//! crate, take no lock, never wait and drop no translator value.
//!
//! **Signals**: one delivery for the process (docs/threads.md, 3.2). The
//! handlers, the pipe and the counts are `super::super::uv_signals`, shared
//! with the single-thread scheduler; the watchers that listen are one list
//! here, the loop's, so a signal reaches the watchers started on any thread,
//! as libuv's one loop delivers to all of its handles.
//!
//! **Networking** (feature `net`, docs/threads.md 0.7): the loop thread also
//! runs libuv's io watchers for `net`'s sockets ([`watch`]: an epoll
//! instance polled with the eventfd and the signal pipe), libuv's pending
//! queue ([`pending`], `uv__io_feed`) and an async queue ([`post`]: the
//! thread pool's completions, `uv_async_send`), in libuv's order within an
//! iteration (`uv_run`, `uv__io_poll`): the pending callbacks; the poll; the
//! io callbacks (batches of up to 1024 events, again while a batch comes
//! back full, at most 48 batches, one when a signal came), then the async
//! queue, then the signals ("Run signal watchers last", `linux.c`); the
//! pending callbacks again (at most 8 rounds); the timers. With a pending
//! callback at the start of the iteration, the poll does not wait (libuv's
//! `can_sleep`). libuv's async watcher is one more io watcher, called in
//! epoll's order among the sockets' events; here the async queue runs
//! after all of them, which is one of the orders epoll can give.
//!
//! **After the task manager's finalization** (`sched::finish` has
//! returned), the loop thread runs no callback any more: no timer, signal,
//! socket, pending or async callback, so no promise resolves (natively one
//! resolved then crashes the exit, LB-27). It looks at the start of each
//! iteration, again after its wait (which may have begun before), and
//! before each callback: a socket's io callback, an async or a pending
//! item, a signal watcher's delivery, a timer. So only the one callback
//! already running when `finish` returns runs to its end (its `sync`
//! dependents included); no other starts, and none that has not run is
//! taken out of its list (a timer's drop could drop its promise, resolving
//! it with `none`). Two atomic loads per callback. It only lets the
//! externs of the exit (a value's drop, a socket's finalizer) take the
//! loop lock. So does the single-thread scheduler, whose loop context does
//! not run after `finish`.
//!
//! What differs from native:
//! - the loop thread is made at the first use, not at startup;
//! - `Loop.configure`'s `blockSigProfSignal` does not block `SIGPROF` in
//!   the loop thread while it polls (`UV_LOOP_BLOCK_SIGNAL`): natively that
//!   only decides which thread runs a `SIGPROF` handler, which no Lean
//!   program can see; `accumulateIdleTime` turns on metrics nothing in Lean
//!   reads. Both succeed and change nothing (case `uvloop/loop_configure`);
//! - occurrences of one signal between two iterations are one delivery
//!   (libuv makes one per occurrence), as in the single-thread `sched::uv`.

use super::task::{glue_and_stack_size, guarded, spawn_thread};
#[cfg(feature = "net")]
use crate::sched::common::{Interest, Ready};
use crate::sched::uv_signals::{self, native_signum, EINVAL};
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use std::collections::BTreeMap;
#[cfg(feature = "net")]
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// The translator's `IO.Promise` as a timer or a signal watcher holds it: a
/// counted reference (cloning it is `lean_inc`, dropping it `lean_dec`).
/// `Send`: the loop thread resolves it and may drop it.
pub trait LoopPromise: Clone + Send + 'static {
    /// `promise_is_resolved`: the promise has a value. Called with a
    /// handle's state lock held: it only reads the promise's slot.
    fn is_resolved(&self) -> bool;
    /// `lean_io_promise_resolve`: resolve it, if no one has, with `()` for a
    /// timer's promise (`IO.Promise Unit`; `value` is 0) or with the signal's
    /// number for a signal watcher's (`IO.Promise Int`). The glue calls
    /// `sched::resolve(id, || store)`; it runs on the loop thread (with the
    /// loop lock held), where the promise's `sync` dependents run.
    fn resolve(&self, value: i64);
}

/// The state of a timer or a signal watcher (`TIMER_STATE_*`,
/// `SIGNAL_STATE_*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Initial,
    Running,
    Finished,
}

/// Whether `next` on a handle in this state needs a new promise: an
/// initial handle, a running repeating one whose promise has resolved (or
/// was dropped), any other without a promise.
fn needs_promise<P: LoopPromise>(state: State, repeating: bool, promise: &Option<P>) -> bool {
    match (state, repeating) {
        (State::Initial, _) => true,
        (State::Running, true) => promise.as_ref().is_none_or(|p| p.is_resolved()),
        _ => promise.is_none(),
    }
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // a panic under these locks aborts (no translator code runs under them)
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// The loop lock

/// Who holds the loop lock, how deep, and how many threads wait for it.
struct Holder {
    owner: Option<ThreadId>,
    depth: u32,
    /// `n_waiters`: the loop thread does not take the lock again while one
    /// is left.
    waiters: u32,
}

/// The loop lock: native `event_loop_t`'s recursive `mutex`, `cond_var` and
/// `n_waiters` (`event_loop.cpp` 66-102; see the module comment).
pub(super) struct LoopLock {
    holder: Mutex<Holder>,
    /// The lock was let go: waiters and the loop thread look again.
    released: Condvar,
}

impl LoopLock {
    pub(super) const fn new() -> LoopLock {
        LoopLock {
            holder: Mutex::new(Holder {
                owner: None,
                depth: 0,
                waiters: 0,
            }),
            released: Condvar::new(),
        }
    }

    /// `event_loop_lock`: at once if free or held by this thread (one level
    /// deeper); otherwise count a waiter, `interrupt` the holder
    /// (`uv_async_send`) and wait until the lock is free.
    pub(super) fn acquire(&self, interrupt: impl FnOnce()) {
        let me = std::thread::current().id();
        let mut h = locked(&self.holder);
        if h.owner == Some(me) {
            h.depth += 1;
            return;
        }
        if h.owner.is_some() {
            h.waiters += 1;
            drop(h);
            interrupt();
            h = locked(&self.holder);
            while h.owner.is_some() {
                h = self
                    .released
                    .wait(h)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            h.waiters -= 1;
        }
        h.owner = Some(me);
        h.depth = 1;
    }

    /// The loop thread's `uv_mutex_lock`, then `while (n_waiters != 0)
    /// uv_cond_wait` (85-91): it takes the lock only when it is free and no
    /// waiter is left.
    pub(super) fn acquire_as_loop(&self) {
        let me = std::thread::current().id();
        let mut h = locked(&self.holder);
        while h.owner.is_some() || h.waiters > 0 {
            h = self
                .released
                .wait(h)
                .unwrap_or_else(PoisonError::into_inner);
        }
        h.owner = Some(me);
        h.depth = 1;
    }

    /// `event_loop_unlock`: one level out; at the outermost, the lock is
    /// free, and the waiters and the loop thread look again.
    pub(super) fn release(&self) {
        let mut h = locked(&self.holder);
        debug_assert_eq!(h.owner, Some(std::thread::current().id()));
        h.depth -= 1;
        if h.depth == 0 {
            h.owner = None;
            drop(h);
            self.released.notify_all();
        }
    }

    /// Whether the calling thread holds the lock (tests).
    #[cfg(test)]
    pub(super) fn held_here(&self) -> bool {
        self.held_by_me()
    }

    /// Whether the calling thread holds the lock (`net`'s debug checks).
    #[cfg(any(test, feature = "net"))]
    fn held_by_me(&self) -> bool {
        locked(&self.holder).owner == Some(std::thread::current().id())
    }
}

// ---------------------------------------------------------------------------
// The loop

/// A loop timer's callback: it gets the timer's own id and the iteration's
/// time (libuv's `loop->time`, cached when the iteration's timers are run).
type Callback = Box<dyn FnOnce(TimerId, Instant) + Send>;

/// An armed loop timer: its deadline and its start serial, the key of
/// `Data::timers` (libuv's timer heap orders by `timeout`, then
/// `start_id`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct TimerId(Instant, u64);

/// A listening signal watcher, as the loop's list holds it (the loop's
/// reference, natively `lean_inc(obj)`).
struct Entry {
    signum: i32,
    repeating: bool,
    /// Creation order (libuv's signal tree orders a signal's watchers by
    /// handle address, that is creation order, after putting repeating ones
    /// first: review RSIOB-08).
    seq: u64,
    /// Its identity (`Signal::id`).
    id: usize,
    watcher: Arc<dyn Listener>,
}

/// What the loop lock protects: the armed timers and the listening signal
/// watchers; with `net`, the io watchers and the pending queue. Its own lock
/// is held only around plain data; what leaves it (a callback, a watcher)
/// is dropped after that lock is let go.
#[derive(Default)]
struct Data {
    /// The armed timers, in deadline order, then start order.
    timers: BTreeMap<(Instant, u64), Callback>,
    /// The last start serial.
    serial: u64,
    /// The listening signal watchers, in the order they started.
    listeners: Vec<Entry>,
    /// libuv's io watchers (`net`'s sockets).
    #[cfg(feature = "net")]
    io: IoWatchers,
    /// libuv's pending queue (`uv__io_feed`): run by the loop thread in its
    /// next iteration, in order.
    #[cfg(feature = "net")]
    pending: VecDeque<Task>,
}

/// The process's loop (native `global_ev`).
struct Loop {
    lock: LoopLock,
    data: Mutex<Data>,
    /// The loop's async eventfd, chosen once when the loop is made, so the
    /// loop polls the one that the externs write (review RT2-04); `None`
    /// when none could be made.
    wake: Option<BorrowedFd<'static>>,
    /// The async queue ([`post`]): callbacks handed to the loop thread from
    /// any thread without the loop lock (libuv's thread pool completions,
    /// `uv__work_done`), with a lock of its own.
    #[cfg(feature = "net")]
    posted: Mutex<VecDeque<Task>>,
}

static LOOP: OnceLock<Loop> = OnceLock::new();
static LOOP_THREAD: Once = Once::new();
/// The loop thread's id, once it runs ([`on_loop_thread`]).
static LOOP_THREAD_ID: OnceLock<ThreadId> = OnceLock::new();

/// Whether the calling thread holds the loop lock (`net`'s unit tests).
#[cfg(all(test, feature = "net"))]
pub(crate) fn lock_held_here() -> bool {
    LOOP.get().is_some_and(|lp| lp.lock.held_by_me())
}

/// How many threads wait for the loop lock (`net`'s unit tests).
#[cfg(all(test, feature = "net"))]
pub(crate) fn lock_waiters() -> u32 {
    LOOP.get().map_or(0, |lp| locked(&lp.lock.holder).waiters)
}

/// Whether the calling thread is the loop thread: a glue that pairs the
/// `thread_start` and `thread_end` of the threads the crate makes can tell
/// it apart there, since the loop thread never ends (no `thread_end`).
pub fn on_loop_thread() -> bool {
    LOOP_THREAD_ID.get() == Some(&std::thread::current().id())
}

/// How long the loop thread waits at most when it has no async eventfd
/// (none could be made when the loop was made: `EMFILE`): an extern then
/// waits for the end of the loop's iteration at most this long.
const NO_WAKE_POLL: Duration = Duration::from_millis(5);

/// The loop, made at the first use, with its thread.
fn the_loop() -> &'static Loop {
    let lp = LOOP.get_or_init(|| Loop {
        lock: LoopLock::new(),
        data: Mutex::new(Data::default()),
        wake: wake_fd(),
        #[cfg(feature = "net")]
        posted: Mutex::new(VecDeque::new()),
    });
    LOOP_THREAD.call_once(|| start_loop_thread(lp));
    lp
}

/// The loop's async eventfd, chosen when the loop is made: native's
/// (`io::startup`, made at startup as libuv's `uv_async_init` makes it)
/// when the glue has opened native's startup descriptors by then, else one
/// of the crate's own, kept for the life of the process; `None` when none
/// can be made.
fn wake_fd() -> Option<BorrowedFd<'static>> {
    #[cfg(feature = "io")]
    if let Some(fd) = crate::io::startup::loop_eventfd() {
        return Some(fd);
    }
    static OWN: OnceLock<OwnedFd> = OnceLock::new();
    if let Some(fd) = OWN.get() {
        return Some(fd.as_fd());
    }
    let fd = rustix::event::eventfd(
        0,
        rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
    )
    .ok()?;
    Some(OWN.get_or_init(|| fd).as_fd())
}

/// `uv_async_send`: wake the loop thread from `poll(2)`, through the loop's
/// eventfd.
fn interrupt(lp: &Loop) {
    if let Some(fd) = lp.wake {
        // `EAGAIN` (the counter full) wakes it all the same
        let _ = rustix::io::write(fd, &1u64.to_ne_bytes());
    }
}

/// The loop lock, held by the calling thread until the guard goes.
pub(crate) struct LoopGuard {
    lp: &'static Loop,
}

impl Drop for LoopGuard {
    fn drop(&mut self) {
        self.lp.lock.release();
    }
}

/// `event_loop_lock(&global_ev)`: every extern's first step (see the module
/// comment), `net`'s too; the loop and its thread are made at the first
/// call.
pub(crate) fn lock() -> LoopGuard {
    let lp = the_loop();
    lp.lock.acquire(|| interrupt(lp));
    LoopGuard { lp }
}

/// The loop thread (`libuv.cpp` 26: `lthread([]() {
/// event_loop_run_loop(&global_ev); })`), detached, with the task manager's
/// stack size. A thread the system cannot make ends the process as an
/// `lthread` that cannot be made does (`failed to create thread`).
///
/// The glue's `thread_start` runs once on it: at its start when the task
/// manager has a glue then, otherwise once a glue appears (the loop made
/// before `sched::start`, by a Lean `initialize` that makes a `Timer`),
/// before the loop runs translator code: each iteration looks for it,
/// after its wait and before its callbacks, until it has called it (one
/// lock of the scheduler per iteration until then; review RT2-05).
fn start_loop_thread(lp: &'static Loop) {
    let (glue, stack_size) = glue_and_stack_size();
    drop(spawn_thread(stack_size, move || {
        let _ = LOOP_THREAD_ID.set(std::thread::current().id());
        #[cfg(feature = "stack-overflow")]
        crate::sched::install_stack_overflow_handler();
        let mut lt = LoopThread {
            glue_started: glue.is_some(),
            #[cfg(feature = "net")]
            events: Vec::with_capacity(EVENTS),
        };
        if let Some(g) = glue {
            guarded("a glue hook", move || g.thread_start());
        }
        guarded("sched::uv's loop thread", || loop {
            lp.lock.acquire_as_loop();
            lp.iterate(&mut lt);
            lp.lock.release();
        });
    }));
}

/// What the loop thread keeps from one iteration to the next.
struct LoopThread {
    /// The glue's `thread_start` has run on it (RT2-05).
    glue_started: bool,
    /// `uv__io_poll`'s buffer of epoll events ([`EVENTS`] of them), made
    /// once: an iteration allocates none for them.
    #[cfg(feature = "net")]
    events: Vec<rustix::event::epoll::Event>,
}

/// How many events one `epoll_wait` takes at most (`uv__io_poll`'s
/// `events[1024]`).
#[cfg(feature = "net")]
const EVENTS: usize = 1024;

/// How many batches of events one iteration takes at most while they come
/// back full (`uv__io_poll`'s `count = 48`).
#[cfg(feature = "net")]
const BATCHES: u32 = 48;

/// The glue's `thread_start` on the loop thread, once, as soon as a glue
/// exists, before the loop runs translator code (RT2-05).
fn glue_check(glue_started: &mut bool) {
    if !*glue_started {
        if let (Some(g), _) = glue_and_stack_size() {
            *glue_started = true;
            guarded("a glue hook", move || g.thread_start());
        }
    }
}

impl Loop {
    /// One iteration (`uv_run(UV_RUN_ONCE)`), with the loop lock held by the
    /// loop thread: wait in `poll(2)` for the earliest timer, the signal pipe
    /// (while a watcher listens) and the async eventfd; then the glue's
    /// `thread_start` if it is still due (`glue_started`); then deliver the
    /// signals that came (`uv__io_poll`'s signal callback) and run the
    /// timers due (`uv__run_timers`). With `net` (module comment): the
    /// pending queue first (then the poll does not wait), the io watchers'
    /// epoll instance polled too; after the poll its callbacks, then the
    /// async queue, then the signals (libuv runs signal watchers last), then
    /// the pending queue again (at most 8 rounds), before the timers. After
    /// the task manager's finalization it only waits for an extern
    /// (`wait_for_externs`, module comment).
    fn iterate(&self, lt: &mut LoopThread) {
        if super::manager_finished() {
            self.wait_for_externs();
            return;
        }
        let glue_started = &mut lt.glue_started;
        // `net`: `uv__run_pending` first, the poll not waiting if it ran
        // something (libuv's `can_sleep`); the glue's `thread_start` before
        // that, as before any callback (RT2-05)
        #[cfg(feature = "net")]
        let can_sleep = {
            glue_check(glue_started);
            !self.run_pending()
        };
        #[cfg(not(feature = "net"))]
        let can_sleep = true;
        let (deadline, listening) = {
            let d = locked(&self.data);
            (d.timers.keys().next().map(|k| k.0), !d.listeners.is_empty())
        };
        #[cfg(feature = "net")]
        let epoll = locked(&self.data).io.epoll;
        #[cfg(not(feature = "net"))]
        let epoll: Option<BorrowedFd<'static>> = None;
        let wake = self.wake;
        let pipe = if listening {
            uv_signals::pipe_read()
        } else {
            None
        };
        let mut timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if wake.is_none() {
            timeout = Some(timeout.map_or(NO_WAKE_POLL, |t| t.min(NO_WAKE_POLL)));
        }
        if !can_sleep {
            timeout = Some(Duration::ZERO);
        }
        // at most `INT_MAX` ms, as libuv clamps it (`uv__io_poll`)
        let ts = timeout.map(|t| {
            let t = t.min(Duration::from_millis(i32::MAX as u64));
            Timespec {
                tv_sec: t.as_secs() as i64,
                tv_nsec: t.subsec_nanos() as i64,
            }
        });
        // `fds[0]` the eventfd, `fds[1]` the pipe, `fds[2]` the epoll
        // instance of `net`'s io watchers, each when present
        let mut fds: Vec<PollFd<'_>> = Vec::with_capacity(3);
        if let Some(w) = wake {
            fds.push(PollFd::from_borrowed_fd(w, PollFlags::IN));
        }
        if let Some(p) = pipe {
            fds.push(PollFd::from_borrowed_fd(p, PollFlags::IN));
        }
        if let Some(e) = epoll {
            fds.push(PollFd::from_borrowed_fd(e, PollFlags::IN));
        }
        // `EINTR` (a signal handler ran on this thread): the iteration goes
        // on, and the signal pipe is looked at below
        let polled = rustix::event::poll(&mut fds, ts.as_ref());
        let ready: Vec<bool> = fds.iter().map(|f| !f.revents().is_empty()).collect();
        drop(fds);
        let woken = wake.is_some() && ready[0];
        let at_pipe = usize::from(wake.is_some());
        let signalled = pipe.is_some() && (polled.is_err() || ready[at_pipe]);
        let io_ready = epoll.is_some() && ready[at_pipe + usize::from(pipe.is_some())];
        if let (true, Some(w)) = (woken, wake) {
            let mut b = [0u8; 8];
            let _ = rustix::io::read(w, &mut b);
        }
        // the wait may have begun before the task manager's finalization
        // ended (`finish` does not interrupt it): no callback after it
        if super::manager_finished() {
            return;
        }
        // the glue's `thread_start`, once, before any callback (RT2-05)
        glue_check(glue_started);
        // `uv__io_poll`'s callbacks: the io watchers' (the sockets', then
        // the async queue's), the signal watchers' last
        #[cfg(feature = "net")]
        {
            if let (true, Some(e)) = (io_ready, epoll) {
                // a signal ends the batches (`if (have_signals != 0) break;`)
                let batches = if signalled { 1 } else { BATCHES };
                self.dispatch_io(e, &mut lt.events, batches);
            }
            self.run_posted();
        }
        #[cfg(not(feature = "net"))]
        let _ = io_ready;
        if signalled {
            self.deliver_signals();
        }
        // `for (r = 0; r < 8 && !uv__queue_empty(&loop->pending_queue);
        // r++) uv__run_pending(loop);`
        #[cfg(feature = "net")]
        for _ in 0..8 {
            if !self.run_pending() {
                break;
            }
        }
        self.run_due_timers();
    }

    /// The iteration after the task manager's finalization: no callback;
    /// wait on the eventfd (an extern's `uv_async_send`) and return, so that
    /// the waiting extern takes the loop lock. Without an eventfd, a short
    /// sleep.
    fn wait_for_externs(&self) {
        match self.wake {
            Some(w) => {
                let mut fds = [PollFd::from_borrowed_fd(w, PollFlags::IN)];
                let _ = rustix::event::poll(&mut fds, None);
                let mut b = [0u8; 8];
                let _ = rustix::io::read(w, &mut b);
            }
            None => std::thread::sleep(NO_WAKE_POLL),
        }
    }

    /// `uv__run_timers` (libuv 1.48): the timers due now, in deadline order,
    /// then start order, each run with its own id; a timer that an earlier
    /// callback stopped or started again is no longer due (its key left
    /// `timers`), and one that becomes due during the run waits for the
    /// next iteration.
    fn run_due_timers(&self) {
        let now = Instant::now();
        let due: Vec<(Instant, u64)> = locked(&self.data)
            .timers
            .range(..=(now, u64::MAX))
            .map(|(k, _)| *k)
            .collect();
        for k in due {
            // before the timer leaves the list: once `finish` has returned
            // it neither runs nor is dropped (its drop could drop the
            // timer's promise, resolving it with `none`)
            if super::manager_finished() {
                return;
            }
            let cb = locked(&self.data).timers.remove(&k);
            if let Some(cb) = cb {
                cb(TimerId(k.0, k.1), now);
            }
        }
    }

    /// The signal pipe's callback: the signals that came, each to the
    /// loop's watchers of it, repeating ones first, then in creation order.
    fn deliver_signals(&self) {
        for s in uv_signals::arrived() {
            let mut ls: Vec<(bool, u64, Arc<dyn Listener>)> = locked(&self.data)
                .listeners
                .iter()
                .filter(|e| e.signum == s)
                .map(|e| (e.repeating, e.seq, e.watcher.clone()))
                .collect();
            ls.sort_by_key(|&(rep, seq, _)| (!rep, seq));
            for (.., l) in ls {
                // no delivery once `finish` has returned (the list keeps
                // the watchers: these are clones)
                if super::manager_finished() {
                    return;
                }
                l.deliver();
            }
        }
    }

    /// `uv_timer_start`, with the loop lock held: `cb` runs on the loop
    /// thread once `deadline` has passed.
    fn timer_start(&self, deadline: Instant, cb: Callback) -> TimerId {
        let mut d = locked(&self.data);
        d.serial += 1;
        let k = (deadline, d.serial);
        d.timers.insert(k, cb);
        TimerId(k.0, k.1)
    }

    /// `uv_timer_stop`, with the loop lock held: the timer's callback, if it
    /// is still armed, for the caller to drop after its locks.
    fn timer_stop(&self, id: TimerId) -> Option<Callback> {
        locked(&self.data).timers.remove(&(id.0, id.1))
    }
}

// ---------------------------------------------------------------------------
// libuv's io watchers, pending queue and async queue, for `net`

/// A callback of the pending queue or the async queue, run once on the loop
/// thread with the loop lock held.
#[cfg(feature = "net")]
pub(crate) type Task = Box<dyn FnOnce() + Send>;

/// A watch's callback: it gets what the loop saw on the descriptor.
#[cfg(feature = "net")]
pub(crate) type WatchCb = Arc<dyn Fn(Ready) + Send + Sync>;

/// A descriptor the loop watches ([`watch`]): a number given once, never
/// reused, which is also the epoll registration's data, so an event of a
/// watch that ended in the same iteration finds nothing. The numbers start
/// above [`WATCH_BASE`], above every descriptor number: native's startup
/// epoll instance, which the loop uses when the glue opened it, has libuv's
/// polling ring registered with its descriptor number as data
/// (`io::startup`), and an event of it finds no watch.
#[cfg(feature = "net")]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct WatchId(u64);

/// Below the first watch's number.
#[cfg(feature = "net")]
const WATCH_BASE: u64 = 1 << 32;

#[cfg(feature = "net")]
struct IoWatch {
    /// The descriptor, held until `unwatch` (a clone of the socket's).
    fd: Arc<OwnedFd>,
    interest: Interest,
    cb: WatchCb,
}

/// libuv's io watchers (`uv__io_t`): the epoll instance (made at the first
/// watch: native's startup one when the glue opened it, else one of the
/// crate's own, kept for the life of the process) and the watches.
#[cfg(feature = "net")]
#[derive(Default)]
struct IoWatchers {
    epoll: Option<BorrowedFd<'static>>,
    serial: u64,
    watches: HashMap<u64, IoWatch>,
}

#[cfg(feature = "net")]
impl IoWatchers {
    fn epoll(&mut self) -> rustix::io::Result<BorrowedFd<'static>> {
        if let Some(e) = self.epoll {
            return Ok(e);
        }
        #[cfg(feature = "io")]
        if let Some(fd) = crate::io::startup::claim_loop_epoll() {
            self.epoll = Some(fd);
            return Ok(fd);
        }
        static OWN: OnceLock<OwnedFd> = OnceLock::new();
        let fd = match OWN.get() {
            Some(fd) => fd.as_fd(),
            None => {
                let fd = rustix::event::epoll::create(rustix::event::epoll::CreateFlags::CLOEXEC)?;
                OWN.get_or_init(|| fd).as_fd()
            }
        };
        self.epoll = Some(fd);
        Ok(fd)
    }
}

/// `uv__io_start` of a new watcher, with the loop lock held: the loop thread
/// runs `cb` whenever `fd` is ready for `interest` (level-triggered, as
/// libuv's), until [`unwatch`]. The loop holds `fd` meanwhile.
#[cfg(feature = "net")]
pub(crate) fn watch(
    fd: Arc<OwnedFd>,
    interest: Interest,
    cb: WatchCb,
) -> rustix::io::Result<WatchId> {
    use rustix::event::epoll;
    let lp = the_loop();
    debug_assert!(
        lp.lock.held_by_me(),
        "lean-runtime: watch without the loop lock"
    );
    let mut d = locked(&lp.data);
    let ep = d.io.epoll()?;
    d.io.serial += 1;
    let id = WATCH_BASE + d.io.serial;
    epoll::add(
        ep,
        &*fd,
        epoll::EventData::new_u64(id),
        interest.epoll_flags(),
    )?;
    d.io.watches.insert(id, IoWatch { fd, interest, cb });
    Ok(WatchId(id))
}

/// `uv__io_start` / `uv__io_stop` of a watcher that keeps waiting for
/// something, with the loop lock held: from now on it waits for `interest`.
#[cfg(feature = "net")]
pub(crate) fn watch_modify(id: WatchId, interest: Interest) -> rustix::io::Result<()> {
    use rustix::event::epoll;
    let lp = the_loop();
    debug_assert!(
        lp.lock.held_by_me(),
        "lean-runtime: watch_modify without the loop lock"
    );
    let mut d = locked(&lp.data);
    let ep = d.io.epoll()?;
    let Some(w) = d.io.watches.get_mut(&id.0) else {
        return Err(rustix::io::Errno::NOENT);
    };
    epoll::modify(
        ep,
        &*w.fd,
        epoll::EventData::new_u64(id.0),
        interest.epoll_flags(),
    )?;
    w.interest = interest;
    Ok(())
}

/// `uv__io_stop` of everything, or `uv__io_close`, with the loop lock held:
/// the watch ends (an event of it still to be dispatched in this iteration
/// finds nothing), its epoll registration goes before the loop lets go of
/// its clone of the descriptor, and its callback is dropped after the
/// loop's data lock.
#[cfg(feature = "net")]
pub(crate) fn unwatch(id: WatchId) {
    let lp = the_loop();
    debug_assert!(
        lp.lock.held_by_me(),
        "lean-runtime: unwatch without the loop lock"
    );
    let gone = {
        let mut d = locked(&lp.data);
        let w = d.io.watches.remove(&id.0);
        if let (Some(w), Some(ep)) = (&w, d.io.epoll) {
            let _ = rustix::event::epoll::delete(ep, &*w.fd);
        }
        w
    };
    drop(gone);
}

/// `uv__io_feed`: run `cb` on the loop thread in its next iteration
/// (libuv's pending queue), with the loop lock held by the caller. A caller
/// on another thread holds the lock, so the loop thread is between two
/// iterations, and its next one runs `cb` first.
#[cfg(feature = "net")]
pub(crate) fn pending(cb: Task) {
    let lp = the_loop();
    debug_assert!(
        lp.lock.held_by_me(),
        "lean-runtime: pending without the loop lock"
    );
    locked(&lp.data).pending.push_back(cb);
}

/// `uv_async_send` with a callback: run `cb` on the loop thread, with the
/// loop lock held, from any thread, without taking the loop lock (libuv's
/// thread pool hands its completions to the loop this way, `uv__work_done`):
/// it queues `cb` and wakes the loop thread through its eventfd. Callbacks
/// run in the order they were posted. One still queued when the process
/// exits never runs and is never dropped.
#[cfg(feature = "net")]
pub(crate) fn post(cb: Task) {
    let lp = the_loop();
    locked(&lp.posted).push_back(cb);
    interrupt(lp);
}

#[cfg(feature = "net")]
impl Loop {
    /// `uv__run_pending`: the callbacks queued until now, in order (one
    /// queued meanwhile waits for the next round); whether any ran.
    fn run_pending(&self) -> bool {
        let n = locked(&self.data).pending.len();
        for _ in 0..n {
            // each taken out only to run: once `finish` has returned, none
            // runs, and none is dropped
            if super::manager_finished() {
                return false;
            }
            let cb = locked(&self.data).pending.pop_front();
            if let Some(cb) = cb {
                cb();
            }
        }
        n > 0
    }

    /// The async queue's callbacks posted until now, in order (each taken
    /// out only to run, as in `run_pending`).
    fn run_posted(&self) {
        let n = locked(&self.posted).len();
        for _ in 0..n {
            if super::manager_finished() {
                return;
            }
            let cb = locked(&self.posted).pop_front();
            if let Some(cb) = cb {
                cb();
            }
        }
    }

    /// `uv__io_poll`'s dispatch, after `poll(2)` saw the epoll instance
    /// ready: the events epoll has (without waiting), in batches of up to
    /// [`EVENTS`] into the loop thread's buffer, again while a batch comes
    /// back full, at most `batches` of them; each event to its watch's
    /// callback if the watch still exists, kept to what the watch waits for
    /// now plus errors and hang-ups (`pe->events &= w->pevents | POLLERR |
    /// POLLHUP`); an event left with nothing is not called back.
    fn dispatch_io(
        &self,
        ep: BorrowedFd<'_>,
        events: &mut Vec<rustix::event::epoll::Event>,
        batches: u32,
    ) {
        use rustix::event::epoll;
        let zero = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        for _ in 0..batches {
            events.clear();
            if epoll::wait(
                ep,
                rustix::buffer::spare_capacity(&mut *events),
                Some(&zero),
            )
            .is_err()
            {
                return;
            }
            let n = events.len();
            for e in events.iter() {
                // no callback once `finish` has returned
                if super::manager_finished() {
                    return;
                }
                let (flags, id) = (e.flags, e.data.u64());
                let ready = Ready::from_epoll(flags);
                let found = {
                    let d = locked(&self.data);
                    d.io.watches.get(&id).map(|w| (w.interest, w.cb.clone()))
                };
                let Some((interest, cb)) = found else {
                    continue;
                };
                if ready.meets(interest) || ready.error || ready.hangup {
                    cb(ready);
                }
            }
            if n < EVENTS {
                return;
            }
        }
    }
}

/// `Std.Internal.UV.Loop.configure` (`lean_uv_event_loop_configure`): it
/// takes the loop lock, and succeeds without a change (module comment).
pub fn loop_configure(accumulate_idle_time: bool, block_sigprof: bool) -> Result<(), i32> {
    let _l = lock();
    let _ = (accumulate_idle_time, block_sigprof);
    Ok(())
}

/// `Std.Internal.UV.Loop.alive` (`lean_uv_event_loop_alive`,
/// `uv_loop_alive`): true. Native Lean's loop always has its async handle
/// (`event_loop_init`), so it is always alive.
pub fn loop_alive() -> bool {
    let _l = lock();
    true
}

// ---------------------------------------------------------------------------
// Timers

struct TimerState<P> {
    timeout: u64,
    repeating: bool,
    state: State,
    /// `m_promise`.
    promise: Option<P>,
    /// The loop's timer while it runs.
    armed: Option<TimerId>,
}

/// `Std.Internal.UV.Timer` (`lean_uv_timer_object`): a translator keeps one
/// in its external object (clones name the same timer). `Send + Sync`.
pub struct Timer<P: LoopPromise>(Arc<Mutex<TimerState<P>>>);

impl<P: LoopPromise> Clone for Timer<P> {
    fn clone(&self) -> Self {
        Timer(self.0.clone())
    }
}

impl<P: LoopPromise> Timer<P> {
    /// `Timer.mk timeout repeating` (`lean_uv_timer_mk`): an initial timer.
    pub fn new(timeout: u64, repeating: bool) -> Timer<P> {
        let _l = lock();
        Timer::initial(timeout, repeating)
    }

    /// A placeholder for the glue only (a value to leave behind in a
    /// `mem::take`-style move), not Lean's `Timer.mk`: an initial one-shot
    /// timer with timeout 0, made without the loop lock, so it never waits
    /// for the loop (nor makes it), as the single-thread `placeholder` never
    /// lets the loop run (reviews AR-22, RT2-08). The glue must never give it
    /// to the program or use it as a timer; its externs act on it as on any
    /// initial timer, and doing so is the glue's error.
    pub fn placeholder() -> Timer<P> {
        Timer::initial(0, false)
    }

    fn initial(timeout: u64, repeating: bool) -> Timer<P> {
        Timer(Arc::new(Mutex::new(TimerState {
            timeout,
            repeating,
            state: State::Initial,
            promise: None,
            armed: None,
        })))
    }

    /// Start the loop's timer, with the loop lock held: due `after` ms from
    /// `from`, then every `repeat` ms if `repeat` is not 0
    /// (`uv_timer_start`, which counts from `loop->time`: an extern passes
    /// its own clock read, the loop's next period the iteration's time). A
    /// deadline past the clock's range never comes (libuv clamps it to
    /// `UINT64_MAX`). The loop holds the timer meanwhile (`lean_inc(obj)`).
    /// Returns the callback of the timer it replaced, for the caller to drop
    /// after its locks.
    fn arm(
        &self,
        lp: &Loop,
        st: &mut TimerState<P>,
        from: Instant,
        after: u64,
        repeat: u64,
    ) -> Option<Callback> {
        let old = st.armed.take().and_then(|id| lp.timer_stop(id));
        if let Some(deadline) = from.checked_add(Duration::from_millis(after)) {
            let me = self.clone();
            st.armed =
                Some(lp.timer_start(deadline, Box::new(move |id, now| me.fire(id, now, repeat))));
        }
        old
    }

    /// `handle_timer_event`, on the loop thread with the loop lock held;
    /// `now` is the iteration's time.
    fn fire(&self, id: TimerId, now: Instant, repeat: u64) {
        let lp = the_loop();
        let p = {
            let mut st = locked(&self.0);
            if st.armed != Some(id) {
                return;
            }
            st.armed = None;
            if st.repeating {
                // libuv starts the next period (`uv_timer_again`) before it
                // calls back, from the iteration's time (`loop->time`; review
                // RT2-09); a repeat of 0 does not repeat
                if repeat != 0 && st.state == State::Running {
                    let _none = self.arm(lp, &mut st, now, repeat, repeat);
                }
                match &st.promise {
                    Some(p) if !p.is_resolved() => Some(p.clone()),
                    _ => None,
                }
            } else {
                // `uv_timer_stop` and finished, the loop letting go of the
                // timer (this callback's own reference, after the call),
                // before the promise resolves: a `sync` dependent of it sees
                // a finished timer, as every later dependent does (LB-20:
                // natively the timer is still running there, and the
                // dependent's `stop` or `cancel` releases it twice)
                st.state = State::Finished;
                st.promise.clone()
            }
        };
        if let Some(p) = p {
            p.resolve(0);
        }
    }

    /// `Timer.next` (`lean_uv_timer_next`); `new_promise` is
    /// `lean_io_promise_new`.
    /// - One-shot: an initial timer starts and gives a new promise, which
    ///   resolves `timeout` ms later; a running or finished one gives that
    ///   promise again, or, after `stop`, a new one that never resolves.
    /// - Repeating: an initial timer starts and gives a new promise, which
    ///   resolves at once (the 0th multiple of `timeout`); a running one gives
    ///   its promise if it has not resolved, else a new one for the next
    ///   tick; a finished one gives its last promise, or, after `stop`, a new
    ///   one that never resolves.
    pub fn next(&self, new_promise: impl FnOnce() -> P) -> P {
        let l = lock();
        // The promise is made with no state lock held: `new_promise` is
        // translator code (review RT2-07). The loop lock is held throughout,
        // so the state changes meanwhile only through that code itself; the
        // state is read again, and a promise it no longer needs is dropped.
        // Each pass reads the promise's state once (`needs`) and acts on that
        // read: the program may resolve it from another thread at any time
        // (review RT2-13).
        let mut make = Some(new_promise);
        let mut fresh: Option<P> = None;
        loop {
            let mut st = locked(&self.0);
            let needs = needs_promise(st.state, st.repeating, &st.promise);
            if needs && fresh.is_none() {
                drop(st);
                fresh = Some((make.take().expect("lean-runtime: a promise made twice"))());
                continue;
            }
            let (p, old_timer, old_promise) = match (st.state, st.repeating) {
                (State::Initial, _) => {
                    let p = fresh.take().expect("made above");
                    st.promise = Some(p.clone());
                    st.state = State::Running;
                    let (after, repeat) = if st.repeating {
                        (0, st.timeout)
                    } else {
                        (st.timeout, 0)
                    };
                    let old = self.arm(l.lp, &mut st, Instant::now(), after, repeat);
                    (p, old, None)
                }
                (State::Running, true) => {
                    let mut old = None;
                    if needs {
                        old = st.promise.replace(fresh.take().expect("made above"));
                    }
                    (st.promise.clone().expect("set above"), None, old)
                }
                _ => match &st.promise {
                    Some(p) => (p.clone(), None, None),
                    None => (fresh.take().expect("made above"), None, None),
                },
            };
            drop(st);
            // after the state's lock: the replaced timer, the resolved
            // promise's reference, a promise no longer needed
            drop((old_timer, old_promise, fresh));
            return p;
        }
    }

    /// `Timer.reset` (`lean_uv_timer_reset`): a running timer's next
    /// resolution moves to `timeout` ms from now, then a repeating one ticks
    /// every `timeout` ms from there; otherwise nothing.
    pub fn reset(&self) {
        let l = lock();
        let mut st = locked(&self.0);
        let old = if st.state == State::Running {
            let t = st.timeout;
            let repeat = if st.repeating { t } else { 0 };
            self.arm(l.lp, &mut st, Instant::now(), t, repeat)
        } else {
            None
        };
        drop(st);
        drop(old);
    }

    /// `Timer.stop` (`lean_uv_timer_stop`): the timer lets go of its promise
    /// (which resolves only through the program's own references now); a
    /// running one stops and is finished.
    pub fn stop(&self) {
        let l = lock();
        let (p, old) = {
            let mut st = locked(&self.0);
            let p = st.promise.take();
            let old = if st.state == State::Running {
                st.state = State::Finished;
                st.armed.take().and_then(|id| l.lp.timer_stop(id))
            } else {
                None
            };
            (p, old)
        };
        drop(old);
        // dropped outside the state's lock: the last reference resolves it
        // with `none`, which may run its `sync` dependents (with the loop
        // lock held, as natively)
        drop(p);
    }

    /// `Timer.cancel` (`lean_uv_timer_cancel`): a running timer with a promise
    /// lets go of it; a one-shot one also stops and is initial again.
    pub fn cancel(&self) {
        let l = lock();
        let (p, old) = {
            let mut st = locked(&self.0);
            if st.state != State::Running || st.promise.is_none() {
                return;
            }
            let p = st.promise.take();
            let old = if st.repeating {
                None
            } else {
                st.state = State::Initial;
                st.armed.take().and_then(|id| l.lp.timer_stop(id))
            };
            (p, old)
        };
        drop(old);
        drop(p);
    }
}

// ---------------------------------------------------------------------------
// Signals

struct SignalState<P> {
    signum: i32,
    repeating: bool,
    state: State,
    promise: Option<P>,
    /// Listening (in the loop's watcher list).
    listening: bool,
    /// Counts the starts of its listening, so that a delivery knows whether
    /// a `sync` dependent stopped or started it again.
    starts: u64,
    /// Its creation order (the order of delivery among one signal's
    /// watchers of the same kind).
    seq: u64,
}

/// `Std.Internal.UV.Signal` (`lean_uv_signal_object`): a translator keeps
/// one in its external object (clones name the same watcher). `Send +
/// Sync`.
pub struct Signal<P: LoopPromise>(Arc<Mutex<SignalState<P>>>);

impl<P: LoopPromise> Clone for Signal<P> {
    fn clone(&self) -> Self {
        Signal(self.0.clone())
    }
}

/// A listening watcher, as the loop's list holds it.
trait Listener: Send + Sync {
    /// `handle_signal_event`, on the loop thread with the loop lock held.
    fn deliver(&self);
}

impl<P: LoopPromise> Listener for Signal<P> {
    fn deliver(&self) {
        let mut st = locked(&self.0);
        if st.state != State::Running || !st.listening {
            return;
        }
        let (signum, start) = (st.signum, st.starts);
        if st.repeating {
            let p = match &st.promise {
                Some(p) if !p.is_resolved() => Some(p.clone()),
                _ => None,
            };
            drop(st);
            if let Some(p) = p {
                p.resolve(signum as i64);
            }
        } else {
            // Finished before the promise resolves: a `sync` dependent of it
            // sees a finished watcher, as every later dependent does (LB-20:
            // natively it is still running there, and the dependent's `stop`
            // or `cancel` releases it twice). libuv calls back, then stops
            // listening (review RSIOB-11).
            st.state = State::Finished;
            let p = st.promise.clone();
            drop(st);
            if let Some(p) = p {
                p.resolve(signum as i64);
            }
            if locked(&self.0).starts == start {
                self.unlisten();
            }
        }
    }
}

impl<P: LoopPromise> Signal<P> {
    /// `Signal.mk signum repeating` (`lean_uv_signal_mk`): an initial
    /// watcher of `signum` (Lean's number), or of no signal if Lean's table
    /// does not have it.
    pub fn new(signum: i32, repeating: bool) -> Signal<P> {
        let _l = lock();
        Signal::initial(native_signum(signum), repeating, uv_signals::next_seq())
    }

    /// A placeholder for the glue only (a value to leave behind in a
    /// `mem::take`-style move), not Lean's `Signal.mk`: an initial one-shot
    /// watcher of no signal (`next` would fail with `UV_EINVAL`), made
    /// without the loop lock, so it never waits for the loop (nor makes it)
    /// (reviews AR-22, RT2-08). The glue must never give it to the program or
    /// use it as a watcher; its externs act on it as on any initial watcher
    /// of no signal, and doing so is the glue's error.
    pub fn placeholder() -> Signal<P> {
        Signal::initial(0, false, 0)
    }

    fn initial(signum: i32, repeating: bool, seq: u64) -> Signal<P> {
        Signal(Arc::new(Mutex::new(SignalState {
            signum,
            repeating,
            state: State::Initial,
            promise: None,
            listening: false,
            starts: 0,
            seq,
        })))
    }

    /// The watcher's identity in the loop's list: its state's address.
    fn id(&self) -> usize {
        Arc::as_ptr(&self.0) as *const () as usize
    }

    /// Stop listening, with the loop lock held (`uv_signal_stop`): off the
    /// loop's list (the loop's reference dropped after the lists' locks),
    /// then the process-wide counts.
    fn unlisten(&self) {
        let (signum, repeating) = {
            let mut st = locked(&self.0);
            if !st.listening {
                return;
            }
            st.listening = false;
            (st.signum, st.repeating)
        };
        let id = self.id();
        let gone = {
            let mut d = locked(&the_loop().data);
            d.listeners
                .iter()
                .position(|e| e.id == id)
                .map(|i| d.listeners.remove(i))
        };
        if gone.is_some() {
            uv_signals::unlisten(signum, !repeating);
        }
        drop(gone);
    }

    /// `Signal.next` (`lean_uv_signal_next`); `new_promise` is
    /// `lean_io_promise_new`. As `Timer::next`: an initial watcher starts
    /// listening and gives a new promise, which resolves with the signal's
    /// number when the next one comes; a repeating one gives a new promise
    /// once the last has resolved. A watcher of no signal fails with
    /// `UV_EINVAL` there, and is left running with that promise, which
    /// never resolves (later `next`s give it), as natively. A failure is a
    /// libuv error code (`UV_EINVAL` is -22).
    pub fn next(&self, new_promise: impl FnOnce() -> P) -> Result<P, i32> {
        let l = lock();
        // The promise is made with no state lock held, and the promise's state
        // read once per pass, as in `Timer::next` (reviews RT2-07, RT2-13).
        let mut make = Some(new_promise);
        let mut fresh: Option<P> = None;
        loop {
            let mut st = locked(&self.0);
            let needs = needs_promise(st.state, st.repeating, &st.promise);
            if needs && fresh.is_none() {
                drop(st);
                fresh = Some((make.take().expect("lean-runtime: a promise made twice"))());
                continue;
            }
            return match (st.state, st.repeating) {
                (State::Initial, _) => {
                    let p = fresh.take().expect("made above");
                    st.promise = Some(p.clone());
                    st.state = State::Running;
                    st.starts += 1;
                    let (signum, repeating, seq) = (st.signum, st.repeating, st.seq);
                    drop(st);
                    if signum == 0 {
                        return Err(-EINVAL);
                    }
                    uv_signals::listen(signum, !repeating).map_err(|e| -e)?;
                    locked(&l.lp.data).listeners.push(Entry {
                        signum,
                        repeating,
                        seq,
                        id: self.id(),
                        watcher: Arc::new(self.clone()),
                    });
                    locked(&self.0).listening = true;
                    Ok(p)
                }
                (State::Running, true) => {
                    let mut old = None;
                    if needs {
                        old = st.promise.replace(fresh.take().expect("made above"));
                    }
                    let p = st.promise.clone().expect("set above");
                    drop(st);
                    drop((old, fresh));
                    Ok(p)
                }
                _ => {
                    let p = match &st.promise {
                        Some(p) => p.clone(),
                        None => fresh.take().expect("made above"),
                    };
                    drop(st);
                    drop(fresh);
                    Ok(p)
                }
            };
        }
    }

    /// `Signal.stop` (`lean_uv_signal_stop`): a running watcher stops
    /// listening, lets go of its promise and is finished. When no watcher of
    /// the signal is left, the signal has its default action again
    /// (`uv__signal_unregister_handler` restores `SIG_DFL`). Never fails
    /// (`uv_signal_stop` gives 0).
    pub fn stop(&self) -> Result<(), i32> {
        let _l = lock();
        let p = {
            let mut st = locked(&self.0);
            if st.state != State::Running {
                return Ok(());
            }
            st.state = State::Finished;
            st.promise.take()
        };
        self.unlisten();
        drop(p);
        Ok(())
    }

    /// `Signal.cancel` (`lean_uv_signal_cancel`): a running watcher with a
    /// promise lets go of it; a one-shot one also stops listening and is
    /// initial again.
    pub fn cancel(&self) {
        let _l = lock();
        let (p, repeating) = {
            let mut st = locked(&self.0);
            if st.state != State::Running || st.promise.is_none() {
                return;
            }
            if !st.repeating {
                st.state = State::Initial;
            }
            (st.promise.take(), st.repeating)
        };
        if !repeating {
            self.unlisten();
        }
        drop(p);
    }
}

#[cfg(test)]
#[path = "uv_tests.rs"]
mod tests;
