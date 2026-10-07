//! The event loop (sched-io): what native Lean's blocked threads wait for in
//! the kernel, and what its libuv loop thread watches, on the scheduler's one
//! thread.
//!
//! Natively a thread that reads an empty pipe, writes a full one or waits for
//! a child blocks in the kernel while the other threads go on, and libuv's
//! loop thread (`event_loop_run_loop`, `src/runtime/uv/event_loop.cpp`) runs
//! the callbacks of timers, signals and sockets. Here, on one thread:
//! - a context that would block waits for its descriptors with [`poll_fds`] (or
//!   [`wait_fd`]): it registers them with the loop's epoll instance and
//!   blocks (`Wait::Io`), and the other contexts run meanwhile;
//! - the loop's own callbacks ([`timer_start`], [`watch`]) run on the *loop
//!   context*, a context of its own as libuv's thread is a thread of its own:
//!   started when a callback is due, ended when none is left;
//! - the hub, when no context can run, waits in `epoll_wait` until a
//!   descriptor is ready or the earliest sleeper or timer is due; the
//!   polling and effect points look without waiting, at most once a
//!   millisecond (`CHECK_EVERY`).
//!
//! A program that has created no task, promise, timer or watch never gets
//! here: its blocking calls stay plain system calls ([`io_cooperative`], the
//! speed floor O12).

use super::ctx::{block, CtxId, Status, Wait};
use super::{alive, with, Sched};
use rustix::event::{epoll, PollFlags, Timespec};
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub use super::common::{Interest, Ready};

// The single-thread loop's own conversions (the definitions are shared with
// threads mode's `sched::uv`, `super::common`).
impl Interest {
    fn poll_flags(self) -> PollFlags {
        let mut f = PollFlags::empty();
        if self.read {
            f |= PollFlags::IN;
        }
        if self.write {
            f |= PollFlags::OUT;
        }
        f
    }

    fn union(self, o: Interest) -> Interest {
        Interest {
            read: self.read || o.read,
            write: self.write || o.write,
        }
    }

    fn is_empty(self) -> bool {
        !self.read && !self.write
    }
}

impl Ready {
    fn from_poll(f: PollFlags) -> Ready {
        let error = f.contains(PollFlags::ERR);
        let hangup = f.contains(PollFlags::HUP);
        // `POLLNVAL` (not an open descriptor): the call will say so
        let nval = f.contains(PollFlags::NVAL);
        Ready {
            read: f.intersects(PollFlags::IN | PollFlags::PRI | PollFlags::RDHUP)
                || error
                || hangup
                || nval,
            write: f.contains(PollFlags::OUT) || error || hangup || nval,
            hangup,
            error: error || nval,
        }
    }
}

/// One descriptor of a [`poll_fds`]: what to wait for, and what was seen.
#[derive(Debug)]
pub struct PollItem<'fd> {
    pub fd: BorrowedFd<'fd>,
    pub interest: Interest,
    /// Filled by [`poll_fds`].
    pub ready: Ready,
}

impl<'fd> PollItem<'fd> {
    pub fn new(fd: BorrowedFd<'fd>, interest: Interest) -> PollItem<'fd> {
        PollItem {
            fd,
            interest,
            ready: Ready::default(),
        }
    }
}

/// A timer of the loop ([`timer_start`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TimerId(u64);

/// A descriptor watched by the loop ([`watch`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WatchId(u64);

/// Set once this process has created a task, a promise, a timer or a
/// watch: until then no blocking call needs to cooperate, and the io
/// layer's stream locks and calls stay plain (one relaxed load).
static COOP: AtomicBool = AtomicBool::new(false);

/// Whether this process has created a task, a promise, a timer or a watch
/// (process-wide, never unset). False: every blocking call is plain.
#[inline]
pub fn coop_possible() -> bool {
    COOP.load(Ordering::Relaxed)
}

#[inline]
pub(crate) fn coop_on() {
    if !COOP.load(Ordering::Relaxed) {
        COOP.store(true, Ordering::Relaxed);
    }
}

/// How often at most the polling and effect points look at the descriptors
/// (one `epoll_wait` with no timeout).
const CHECK_EVERY: Duration = Duration::from_millis(1);

/// The loop's epoll instance: native Lean's, from `io::startup` (libuv's
/// loop descriptor, kept open for the life of the process), or one of its
/// own.
#[derive(Clone)]
enum EpollFd {
    #[cfg_attr(not(feature = "io"), allow(dead_code))]
    Startup(BorrowedFd<'static>),
    Own(Rc<OwnedFd>),
}

impl EpollFd {
    fn fd(&self) -> BorrowedFd<'_> {
        match self {
            EpollFd::Startup(f) => *f,
            EpollFd::Own(f) => f.as_fd(),
        }
    }
}

/// A watch: a callback run on the loop context when its descriptor is
/// ready.
struct Watch {
    id: u64,
    interest: Interest,
    cb: Rc<dyn Fn(Ready)>,
    /// The descriptor, held until `unwatch` (the glue's clone of it).
    fd: Box<dyn AsFd>,
    /// A call is queued (`Due::Fd`) or running: the watch is out of epoll's
    /// set until the call returns (`watch_done`), so a callback that blocks
    /// while the descriptor stays ready does not make the hub spin (review
    /// RSIO-02).
    busy: bool,
}

/// One descriptor registered with epoll.
#[derive(Default)]
struct Reg {
    /// What epoll has for it (empty: not added).
    added: epoll::EventFlags,
    /// Contexts blocked on it (`Wait::Io`), with what each waits for.
    waiters: Vec<(CtxId, Interest)>,
    watch: Option<Watch>,
}

impl Reg {
    fn want(&self) -> Interest {
        let mut i = match &self.watch {
            Some(w) if !w.busy => w.interest,
            _ => Interest::default(),
        };
        for &(_, w) in &self.waiters {
            i = i.union(w);
        }
        i
    }

    fn is_empty(&self) -> bool {
        self.waiters.is_empty() && self.watch.is_none()
    }
}

/// A callback due on the loop context.
pub(crate) enum Due {
    /// A timer's call, with the time of the look that found it due (the
    /// loop's time for its callback: `Reactor::fired_at`, review HU-06).
    Timer(u64, Instant),
    /// A watch's call (descriptor number, watch id): its readiness is
    /// looked at again right before the call (`next_due`).
    Fd(i32, u64),
}

/// The loop's state, one per scheduler (`Sched::ev`).
#[derive(Default)]
pub(crate) struct Reactor {
    ep: Option<EpollFd>,
    regs: HashMap<i32, Reg>,
    timers: BinaryHeap<Reverse<(Instant, u64)>>,
    timer_cbs: HashMap<u64, Rc<dyn Fn()>>,
    /// The descriptor (its number) of each watch.
    watches: HashMap<u64, i32>,
    serial: u64,
    due: VecDeque<Due>,
    /// The loop context, while it runs.
    loop_ctx: Option<CtxId>,
    last_check: Option<Instant>,
    /// The loop's time for the timer callback that runs now
    /// ([`loop_time`]): libuv's `loop->time` of the iteration that runs it.
    fired_at: Option<Instant>,
}

impl Drop for Reactor {
    fn drop(&mut self) {
        // At thread exit the callbacks are not dropped: they hold Lean values
        // whose destructors would call back into the scheduler during its
        // destruction (as `Tasks` forgets its jobs).
        for (_, cb) in self.timer_cbs.drain() {
            std::mem::forget(cb);
        }
        for (_, r) in self.regs.drain() {
            if let Some(w) = r.watch {
                std::mem::forget(w.cb);
                std::mem::forget(w.fd);
            }
        }
    }
}

impl Reactor {
    /// Something can still happen: a descriptor is registered, a timer is
    /// pending, or a callback is due.
    pub(crate) fn active(&self) -> bool {
        !self.regs.is_empty() || !self.timer_cbs.is_empty() || !self.due.is_empty()
    }

    /// A descriptor is registered (an event may come at any time).
    pub(crate) fn has_regs(&self) -> bool {
        !self.regs.is_empty()
    }

    /// The loop context, while it runs (`finish` does not wait for it).
    pub(crate) fn loop_ctx(&self) -> Option<CtxId> {
        self.loop_ctx
    }

    /// A callback is due and no loop context runs.
    pub(crate) fn wants_loop(&self) -> bool {
        !self.due.is_empty() && self.loop_ctx.is_none()
    }

    /// The earliest pending timer.
    pub(crate) fn next_timer(&mut self) -> Option<Instant> {
        while let Some(&Reverse((d, id))) = self.timers.peek() {
            if self.timer_cbs.contains_key(&id) {
                return Some(d);
            }
            self.timers.pop();
        }
        None
    }

    fn epoll(&mut self) -> rustix::io::Result<EpollFd> {
        if let Some(e) = &self.ep {
            return Ok(e.clone());
        }
        #[cfg(feature = "io")]
        if let Some(fd) = crate::io::startup::claim_loop_epoll() {
            self.ep = Some(EpollFd::Startup(fd));
            return Ok(EpollFd::Startup(fd));
        }
        let fd = epoll::create(epoll::CreateFlags::CLOEXEC)?;
        let e = EpollFd::Own(Rc::new(fd));
        self.ep = Some(e.clone());
        Ok(e)
    }

    /// Make epoll's registration of `fd` match what its waiters and watch
    /// want.
    fn sync(&mut self, fd: BorrowedFd<'_>) -> rustix::io::Result<()> {
        self.sync_raw(fd.as_raw_fd(), Some(fd))
    }

    /// `sync` of the registration of descriptor number `raw`, through
    /// `given` or the descriptor its watch holds. An empty registration is
    /// dropped before anything else, so a failing `epoll_create` leaves none
    /// behind (review RSIO-06).
    fn sync_raw(&mut self, raw: i32, given: Option<BorrowedFd<'_>>) -> rustix::io::Result<()> {
        let Some(r) = self.regs.get(&raw) else {
            return Ok(());
        };
        let want = r.want().epoll_flags();
        let added = r.added;
        if want == added {
            if r.is_empty() {
                self.regs.remove(&raw);
            }
            return Ok(());
        }
        let ep = match self.epoll() {
            Ok(e) => e,
            Err(e) => {
                if self.regs.get(&raw).is_some_and(Reg::is_empty) {
                    self.regs.remove(&raw);
                }
                return Err(e);
            }
        };
        let r = self.regs.get(&raw).expect("looked up above");
        let held = r.watch.as_ref().map(|w| w.fd.as_fd());
        let Some(fd) = given.or(held) else {
            // No descriptor to name it with: only a registration whose last
            // user is gone, which `unwatch` and `remove_waiters` delete with
            // theirs.
            return Ok(());
        };
        let data = epoll::EventData::new_u64(raw as u64);
        let res = if want.is_empty() {
            epoll::delete(ep.fd(), fd).or_else(|e| match e {
                rustix::io::Errno::NOENT | rustix::io::Errno::BADF => Ok(()),
                e => Err(e),
            })
        } else if added.is_empty() {
            epoll::add(ep.fd(), fd, data, want).or_else(|e| match e {
                rustix::io::Errno::EXIST => epoll::modify(ep.fd(), fd, data, want),
                e => Err(e),
            })
        } else {
            epoll::modify(ep.fd(), fd, data, want).or_else(|e| match e {
                rustix::io::Errno::NOENT => epoll::add(ep.fd(), fd, data, want),
                e => Err(e),
            })
        };
        res?;
        let r = self.regs.get_mut(&raw).expect("looked up above");
        r.added = want;
        if r.is_empty() {
            self.regs.remove(&raw);
        }
        Ok(())
    }

    fn add_waiters(&mut self, me: CtxId, items: &[PollItem<'_>]) -> rustix::io::Result<()> {
        for (k, it) in items.iter().enumerate() {
            if it.interest.is_empty() {
                continue;
            }
            let raw = it.fd.as_raw_fd();
            self.regs
                .entry(raw)
                .or_default()
                .waiters
                .push((me, it.interest));
            if let Err(e) = self.sync(it.fd) {
                self.remove_waiters(me, &items[..=k]);
                return Err(e);
            }
        }
        Ok(())
    }

    fn remove_waiters(&mut self, me: CtxId, items: &[PollItem<'_>]) {
        for it in items {
            let raw = it.fd.as_raw_fd();
            if let Some(r) = self.regs.get_mut(&raw) {
                if let Some(p) = r.waiters.iter().position(|&(c, _)| c == me) {
                    r.waiters.swap_remove(p);
                }
                let _ = self.sync(it.fd);
            }
        }
    }

    /// The pending timers due by `by`, in their order (deadline, then start
    /// order, as libuv's heap), queued for the loop context with the look's
    /// time `look`. Whether it queued one.
    fn take_due_timers(&mut self, by: Instant, look: Instant) -> bool {
        let mut took = false;
        while let Some(&Reverse((d, id))) = self.timers.peek() {
            if d > by {
                break;
            }
            self.timers.pop();
            if self.timer_cbs.contains_key(&id) {
                self.due.push_back(Due::Timer(id, look));
                took = true;
            }
        }
        took
    }

    /// The next due callback for the loop context, or `None` (and the loop
    /// context ends: `loop_ctx` is cleared in the same step).
    fn next_due(&mut self) -> Option<Run> {
        while let Some(d) = self.due.pop_front() {
            match d {
                Due::Timer(id, look) => {
                    if let Some(cb) = self.timer_cbs.remove(&id) {
                        return Some(Run::Timer(cb, look));
                    }
                }
                Due::Fd(raw, id) => {
                    let Some(w) = self.regs.get(&raw).and_then(|r| r.watch.as_ref()) else {
                        continue;
                    };
                    if w.id != id {
                        continue;
                    }
                    // Still ready? Another context may have drained it since
                    // the loop saw it (review RSIO-02). poll(2) reports
                    // `POLLERR` and `POLLHUP` whatever is asked, and either
                    // counts as ready (a failed connect, an end of file).
                    let mut it = [PollItem::new(w.fd.as_fd(), w.interest)];
                    let ready = match plain_poll(&mut it, Some(Duration::ZERO)) {
                        Ok(_) => it[0].ready,
                        Err(_) => Ready::default(),
                    };
                    if ready.meets(w.interest) {
                        return Some(Run::Fd(w.cb.clone(), raw, id, ready));
                    }
                    self.watch_done(raw, id);
                }
            }
        }
        self.loop_ctx = None;
        None
    }

    /// A watch's call has returned (or was skipped): it is armed again, with
    /// its current interest, unless it was unwatched meanwhile.
    fn watch_done(&mut self, raw: i32, id: u64) {
        match self.regs.get_mut(&raw).and_then(|r| r.watch.as_mut()) {
            Some(w) if w.id == id => w.busy = false,
            _ => return,
        }
        let _ = self.sync_raw(raw, None);
    }
}

enum Run {
    /// A timer's call: its callback and the time of the look that found it
    /// due.
    Timer(Rc<dyn Fn()>, Instant),
    /// A watch's call: its callback, descriptor number, id and readiness.
    Fd(Rc<dyn Fn(Ready)>, i32, u64, Ready),
}

impl Sched {
    /// The events epoll reported: wake the contexts blocked on them, queue
    /// the calls of the watches, each out of epoll's set until its call has
    /// returned.
    fn ev_dispatch(&mut self, events: &[epoll::Event]) {
        let mut disarm = Vec::new();
        for e in events {
            let raw = e.data.u64() as i32;
            let ready = Ready::from_epoll(e.flags);
            let Some(r) = self.ev.regs.get_mut(&raw) else {
                continue;
            };
            let wake: Vec<CtxId> = r
                .waiters
                .iter()
                .filter(|&&(_, i)| ready.meets(i))
                .map(|&(c, _)| c)
                .collect();
            if let Some(w) = r.watch.as_mut() {
                if !w.busy && ready.meets(w.interest) {
                    w.busy = true;
                    self.ev.due.push_back(Due::Fd(raw, w.id));
                    disarm.push(raw);
                }
            }
            for c in wake {
                let x = &self.cx.ctxs[c];
                if x.status == Status::Blocked && matches!(x.wait, Wait::Io(_)) {
                    self.wake(c);
                }
            }
        }
        for raw in disarm {
            let _ = self.ev.sync_raw(raw, None);
        }
    }

    /// Look at the loop without waiting: (at most once per `CHECK_EVERY`,
    /// unless `force`) the descriptors, then the timers due by `now`.
    /// Whether it queued a timer's call (an effect point lets the loop
    /// context go first then, as a due sleeper: review HU-01).
    pub(crate) fn ev_check(&mut self, now: Instant, force: bool) -> bool {
        self.ev_check_by(now, now, force)
    }

    /// [`Sched::ev_check`] that takes only the timers due by `timers_by`
    /// (`catch_up`, review HU-03). Within one look the descriptors' events
    /// (a signal watcher's pipe among them) are queued before the timers
    /// due, as libuv runs `uv__io_poll`'s callbacks before
    /// `uv__run_timers` in one iteration, and as threads mode does (review
    /// HU-04).
    pub(crate) fn ev_check_by(&mut self, now: Instant, timers_by: Instant, force: bool) -> bool {
        if !self.ev.active() {
            return false;
        }
        self.ev_check_fds(now, force);
        self.ev.take_due_timers(timers_by, now)
    }

    /// The descriptors part of [`Sched::ev_check_by`].
    fn ev_check_fds(&mut self, now: Instant, force: bool) {
        if self.ev.regs.is_empty() {
            return;
        }
        if !force {
            if let Some(l) = self.ev.last_check {
                if now.saturating_duration_since(l) < CHECK_EVERY {
                    return;
                }
            }
        }
        self.ev.last_check = Some(now);
        let Some(ep) = self.ev.ep.clone() else {
            return;
        };
        let mut events: Vec<epoll::Event> = Vec::with_capacity(32);
        let zero = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if epoll::wait(
            ep.fd(),
            rustix::buffer::spare_capacity(&mut events),
            Some(&zero),
        )
        .is_ok()
        {
            self.ev_dispatch(&events);
        }
    }

    /// Start the loop context if a callback is due and none runs.
    /// Whether context `n` is the event loop's (`loop_main`): for `slots`
    /// (with `io`) and `IO.getTID`'s loop thread (`tid_offset`).
    pub(crate) fn is_loop_ctx(&self, n: CtxId) -> bool {
        self.ev.loop_ctx == Some(n)
    }

    pub(crate) fn ev_start_loop(&mut self) -> bool {
        if self.ev.wants_loop() {
            // libuv's loop thread has 1 GiB natively, whatever
            // `LEAN_STACK_SIZE_KB` says (hunt HSK-03)
            let size = self.cx.loop_stack_size();
            let c = self.start_context(loop_main, size);
            self.ev.loop_ctx = Some(c);
            return true;
        }
        false
    }
}

/// The hub has nothing to run until `deadline` (a sleeper's or a timer's),
/// or ever: wait for a descriptor or the deadline, in `epoll_wait` when a
/// descriptor is registered, otherwise in a sleep (or forever: a deadlocked
/// native program waits forever too). Runs on `main`'s stack, in the hub; it
/// never blocks a context or switches.
pub(crate) fn idle(deadline: Option<Instant>) {
    let ep = with(|s| {
        if s.ev.has_regs() {
            s.ev.ep.clone()
        } else {
            None
        }
    });
    let now = Instant::now();
    let Some(ep) = ep else {
        match deadline {
            Some(d) => std::thread::sleep(d.saturating_duration_since(now)),
            None => super::hang_thread(),
        }
        return;
    };
    // At most `INT_MAX` ms, as libuv clamps it (`uv__io_poll`): a longer
    // timeout would take rustix to `epoll_pwait2`, which kernels before 5.11
    // refuse (review RSIOB-10). The hub looks again when this returns.
    let timeout = deadline.map(|d| {
        let t = d
            .saturating_duration_since(now)
            .min(Duration::from_millis(i32::MAX as u64));
        Timespec {
            tv_sec: t.as_secs() as i64,
            tv_nsec: t.subsec_nanos() as i64,
        }
    });
    let mut events: Vec<epoll::Event> = Vec::with_capacity(32);
    // `EINTR` (a signal handler ran on this thread): the hub looks again.
    let r = epoll::wait(
        ep.fd(),
        rustix::buffer::spare_capacity(&mut events),
        timeout.as_ref(),
    );
    with(|s| {
        s.ev.last_check = Some(Instant::now());
        if r.is_ok() {
            s.ev_dispatch(&events);
        }
    });
}

/// The loop context's function: run the due callbacks one after the other,
/// in the order they became due, then end. A callback may block (a promise's
/// `sync` dependent running there); what becomes due meanwhile runs after it,
/// as on libuv's one thread.
pub(crate) fn loop_main() {
    /// A Rust panic in a callback ends the loop context: the next due
    /// callback starts a new one, and the watch whose call it was is armed
    /// again.
    struct Reset(Option<(i32, u64)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            if alive() {
                let w = self.0;
                with(|s| {
                    s.ev.loop_ctx = None;
                    if let Some((raw, id)) = w {
                        s.ev.watch_done(raw, id);
                    }
                });
            }
        }
    }
    let mut reset = Reset(None);
    // The loop's time for a timer's callback (`fired_at`, review HU-06): for
    // what was due when this context first ran, the time of the look that
    // found it due, as libuv's `loop->time` of the iteration that runs it
    // (natively the loop thread runs it then; here other contexts able to
    // run may go first); for what a look found due while this context ran
    // a callback, the time it takes it, as natively the busy loop thread
    // looks again only then.
    let first = Instant::now();
    while let Some(run) = with(|s| s.ev.next_due()) {
        match run {
            Run::Timer(cb, look) => {
                let t = if look < first { look } else { Instant::now() };
                with(|s| s.ev.fired_at = Some(t));
                cb();
                with(|s| s.ev.fired_at = None);
            }
            Run::Fd(cb, raw, id, ready) => {
                reset.0 = Some((raw, id));
                cb(ready);
                drop(cb);
                reset.0 = None;
                with(|s| s.ev.watch_done(raw, id));
            }
        }
    }
    std::mem::forget(reset);
}

// ---------------------------------------------------------------------------
// The API

thread_local! {
    /// The depth of the no-suspend scopes this thread is in
    /// ([`enter_no_suspend`]).
    static NO_SUSPEND: Cell<u32> = const { Cell::new(0) };
}

/// Enter a no-suspend scope (nestable): until the matching
/// [`leave_no_suspend`], the io layer's blocking calls on this thread are
/// plain system calls that block the thread, never a suspension of the
/// running context. Both translators mark their free and drop paths with it:
/// dropping a handle's last reference closes it, and its flush may wait for
/// a full pipe (review RSIO-03); lean2rr's runtime must not suspend inside a
/// free. A thread-local counter, no lock, no allocation; it is read only
/// where a cooperative call would wait. The scheduler's own waits (a
/// promise, a sleep, a `Std.Sync` lock) are not affected.
#[inline]
pub fn enter_no_suspend() {
    NO_SUSPEND.with(|n| n.set(n.get() + 1));
}

/// Leave the innermost no-suspend scope ([`enter_no_suspend`]). It never
/// suspends, never writes and never waits: a decrement of the thread-local
/// counter, so it is safe in any `Drop`, also during a panic's unwinding.
/// A stream whose drop in the scope found its pipe full has already been
/// handed to an internal writer thread (`io::coop::hand_off`, AR-8).
#[inline]
pub fn leave_no_suspend() {
    let _ = NO_SUSPEND.try_with(|n| n.set(n.get().saturating_sub(1)));
}

/// The running context's no-suspend depth, set aside while it is switched
/// away (`switch_away`): the other contexts run outside its scope, and it is
/// in its scope again when it goes on (review RSIO-10).
pub(crate) fn park_no_suspend() -> NoSuspendParked {
    NoSuspendParked(NO_SUSPEND.try_with(|n| n.replace(0)).unwrap_or(0))
}

/// A suspended context's no-suspend depth ([`park_no_suspend`]), given back
/// when it goes on, or when a panic unwinds its switch.
pub(crate) struct NoSuspendParked(u32);

impl Drop for NoSuspendParked {
    fn drop(&mut self) {
        let d = self.0;
        let _ = NO_SUSPEND.try_with(|n| n.set(d));
    }
}

/// A no-suspend scope as a guard ([`enter_no_suspend`] now, and
/// [`leave_no_suspend`] when dropped).
#[must_use = "the scope ends when the guard is dropped"]
pub struct NoSuspendGuard {
    _not_send: std::marker::PhantomData<*const ()>,
}

/// [`enter_no_suspend`], ended when the guard is dropped.
#[inline]
pub fn no_suspend() -> NoSuspendGuard {
    enter_no_suspend();
    NoSuspendGuard {
        _not_send: std::marker::PhantomData,
    }
}

impl Drop for NoSuspendGuard {
    #[inline]
    fn drop(&mut self) {
        leave_no_suspend();
    }
}

/// Whether this thread is in a no-suspend scope.
#[inline]
pub fn in_no_suspend() -> bool {
    NO_SUSPEND.try_with(|n| n.get() > 0).unwrap_or(true)
}

/// Whether this thread is in a no-suspend scope, false once the thread's
/// locals are gone: the wait cores' test (W3 of `docs/sched.md`, "The wait
/// cores"), where a wait at thread teardown hangs the thread (W6) instead
/// of panicking, and the debug checks of `resolve` and `run_deferred` (R2).
/// [`in_no_suspend`] answers true there instead, so that the io layer takes
/// its plain path.
#[inline]
pub(crate) fn in_no_suspend_scope() -> bool {
    NO_SUSPEND.try_with(|n| n.get() > 0).unwrap_or(false)
}

/// Whether a blocking call on this thread must let other contexts run
/// (sched-io): the task manager runs and another context exists, a task is
/// queued, or the loop has a descriptor, a timer or a due callback, and the
/// thread is not in a no-suspend scope. Otherwise the call blocks the thread
/// as a plain system call does, which then costs nothing more than before
/// sched-io.
pub fn io_cooperative() -> bool {
    if !coop_possible() || !alive() || in_no_suspend() {
        return false;
    }
    with(|s| {
        s.tk.started
            && (s.cx.live() > 1 || s.has_queued() || s.picked_io_pending() || s.ev.active())
    })
}

fn to_timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: d.as_secs().min(i64::MAX as u64) as i64,
        tv_nsec: d.subsec_nanos() as i64,
    }
}

/// `poll(2)` on `items` with `timeout`, filling each item's `ready`; `EINTR`
/// is retried.
fn plain_poll(items: &mut [PollItem<'_>], timeout: Option<Duration>) -> rustix::io::Result<usize> {
    let mut fds: Vec<rustix::event::PollFd<'_>> = items
        .iter()
        .map(|it| rustix::event::PollFd::from_borrowed_fd(it.fd, it.interest.poll_flags()))
        .collect();
    let ts = timeout.map(to_timespec);
    let n = loop {
        match rustix::event::poll(&mut fds, ts.as_ref()) {
            Err(rustix::io::Errno::INTR) => continue,
            r => break r?,
        }
    };
    for (it, f) in items.iter_mut().zip(&fds) {
        it.ready = Ready::from_poll(f.revents());
    }
    Ok(n)
}

/// `poll(2)` for the calling context: returns once one of `items` is ready
/// (the number ready, each item's `ready` filled), or after `timeout` (0).
/// In a program with other contexts (`io_cooperative`) the context blocks
/// and the others run meanwhile; otherwise it is a plain `poll(2)`, which
/// blocks the thread. An item with an empty interest is only reported on
/// errors, as `poll(2)` does.
///
/// The caller may hold the io layer's stream locks across it: every
/// suspension records them as held by the suspended context, and another
/// context that wants one waits for it (review RSIO-01). Any other state the
/// caller holds that another context may need, it releases first.
pub fn poll_fds(
    items: &mut [PollItem<'_>],
    timeout: Option<Duration>,
) -> rustix::io::Result<usize> {
    if !io_cooperative() {
        return plain_poll(items, timeout);
    }
    let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
    loop {
        let n = plain_poll(items, Some(Duration::ZERO))?;
        if n > 0 || timeout == Some(Duration::ZERO) {
            return Ok(n);
        }
        let now = Instant::now();
        if deadline.is_some_and(|d| now >= d) {
            return Ok(0);
        }
        let me = super::current_context();
        if with(|s| s.ev.add_waiters(me, items)).is_err() {
            // epoll refuses (no memory, `max_user_watches`): the plain wait
            return plain_poll(items, deadline.map(|d| d.saturating_duration_since(now)));
        }
        coop_on();
        // The registrations go when the wait ends, also if a Rust panic of
        // another context goes on here (on `main`'s context, S6 of
        // docs/sched.md).
        let unregister = Unregister(me, items);
        block(Wait::Io(deadline));
        drop(unregister);
    }
}

/// Ends a context's registrations of `poll_fds` (see there).
struct Unregister<'a, 'fd>(CtxId, &'a [PollItem<'fd>]);

impl Drop for Unregister<'_, '_> {
    fn drop(&mut self) {
        if alive() {
            with(|s| s.ev.remove_waiters(self.0, self.1));
        }
    }
}

/// Wait until `fd` is ready for `interest` ([`poll_fds`] of one descriptor, no
/// timeout): what was seen.
pub fn wait_fd(fd: BorrowedFd<'_>, interest: Interest) -> rustix::io::Result<Ready> {
    let mut it = [PollItem::new(fd, interest)];
    poll_fds(&mut it, None)?;
    Ok(it[0].ready)
}

/// Run `cb` on the loop context once `deadline` has passed (libuv's timer
/// heap: due timers run in deadline order, then in start order). It runs
/// when the hub or a polling or effect point finds it due.
pub fn timer_start(deadline: Instant, cb: Rc<dyn Fn()>) -> TimerId {
    coop_on();
    with(|s| {
        s.ev.serial += 1;
        let id = s.ev.serial;
        s.ev.timers.push(Reverse((deadline, id)));
        s.ev.timer_cbs.insert(id, cb);
        TimerId(id)
    })
}

/// Stop a timer whose callback has not run yet (a due one whose call has
/// not started is stopped too; the UV externs first let the loop run what
/// is due, `catch_up`, as natively the loop thread runs a due timer before
/// an extern acts). Whether it was pending. Its callback is dropped here,
/// outside the scheduler's state.
pub fn timer_stop(id: TimerId) -> bool {
    if !alive() {
        return false;
    }
    let cb = with(|s| s.ev.timer_cbs.remove(&id.0));
    let was = cb.is_some();
    drop(cb);
    was
}

/// Watch `fd`: run `cb` on the loop context whenever it is ready for
/// `interest` (level-triggered, as libuv's io watchers). The reactor keeps
/// `fd` (a clone of the glue's `Rc<OwnedFd>`, say) until [`unwatch`], so the
/// descriptor stays open and registered while watched; one watch per
/// descriptor, `EEXIST` if it has one.
///
/// From the moment a call is queued until it returns, the watch is out of
/// epoll's set: a callback that blocks while the descriptor stays ready does
/// not make the hub spin, and the next call is queued only once the
/// descriptor is seen ready again after the return. Right before a queued
/// call runs, `poll(2)` checks the descriptor again for the watch's current
/// interest (`POLLERR` and `POLLHUP` count as ready); a call whose
/// descriptor is no longer ready is skipped. Inside its own callback a
/// watch may be modified ([`watch_modify`]: the interest used when it is
/// armed again) or ended ([`unwatch`]: it is not armed again, and its
/// descriptor is let go at once).
pub fn watch<F: AsFd + 'static>(
    fd: F,
    interest: Interest,
    cb: Rc<dyn Fn(Ready)>,
) -> rustix::io::Result<WatchId> {
    coop_on();
    let raw = fd.as_fd().as_raw_fd();
    let r = with(move |s| {
        let w = Watch {
            id: 0,
            interest,
            cb,
            fd: Box::new(fd),
            busy: false,
        };
        if s.ev.regs.get(&raw).is_some_and(|r| r.watch.is_some()) {
            // dropped by the caller, outside the scheduler's state (review
            // RSIO-11)
            return Err((rustix::io::Errno::EXIST, Some(w)));
        }
        s.ev.serial += 1;
        let id = s.ev.serial;
        s.ev.regs.entry(raw).or_default().watch = Some(Watch { id, ..w });
        match s.ev.sync_raw(raw, None) {
            Ok(()) => {
                s.ev.watches.insert(id, raw);
                Ok(WatchId(id))
            }
            Err(e) => {
                let w = s.ev.regs.get_mut(&raw).and_then(|r| r.watch.take());
                if let Some(w) = &w {
                    let _ = s.ev.sync_raw(raw, Some(w.fd.as_fd()));
                }
                Err((e, w))
            }
        }
    });
    // A refused watch's callback and descriptor are dropped here, outside
    // the scheduler's state.
    r.map_err(|(e, w)| {
        drop(w);
        e
    })
}

/// Change what a watch waits for (from inside its own callback too: the
/// interest it is armed with when the call returns). `ENOENT` for a watch
/// that has ended.
pub fn watch_modify(id: WatchId, interest: Interest) -> rustix::io::Result<()> {
    with(|s| {
        let Some(&raw) = s.ev.watches.get(&id.0) else {
            return Err(rustix::io::Errno::NOENT);
        };
        match s.ev.regs.get_mut(&raw).and_then(|r| r.watch.as_mut()) {
            Some(w) if w.id == id.0 => w.interest = interest,
            _ => return Err(rustix::io::Errno::NOENT),
        }
        s.ev.sync_raw(raw, None)
    })
}

/// End a watch, from anywhere, its own callback included: a call already
/// queued does not run, and a running one is not followed by another. The
/// reactor lets go of the descriptor and the callback before this returns
/// (a running callback keeps its own reference until it returns), so a
/// descriptor the glue no longer holds closes here.
pub fn unwatch(id: WatchId) {
    if !alive() {
        return;
    }
    let w = with(|s| {
        let raw = s.ev.watches.remove(&id.0)?;
        let r = s.ev.regs.get_mut(&raw)?;
        if r.watch.as_ref().is_none_or(|w| w.id != id.0) {
            return None;
        }
        let w = r.watch.take()?;
        let _ = s.ev.sync_raw(raw, Some(w.fd.as_fd()));
        Some(w)
    });
    drop(w);
}

/// Natively every `Std.Internal.UV` extern takes the loop's lock
/// (`event_loop_lock`), which makes the loop thread finish its current
/// iteration first: the callbacks of the timers due and the signals and
/// descriptors that arrived run before the extern acts (review RSIOB-04).
/// Here: look at the loop now (the descriptors too, without the
/// millisecond's throttle), and let the loop context run once, so what is
/// due runs first: one iteration, as natively, however much keeps becoming
/// due (review RSIOB-13). Nothing from the loop context itself (natively the
/// loop thread already holds its lock, which is recursive: a callback's
/// `sync` dependent calling an extern runs no iteration), nor in a
/// no-suspend scope. The yield lets every context able to run go first, not
/// only the loop's: each extern is a scheduling point for all of them, an
/// order native's threads allow too (RSIOB-14).
pub(crate) fn catch_up() {
    if !coop_possible() || !alive() || in_no_suspend() {
        return;
    }
    let go = with(|s| {
        if !s.tk.started || !s.ev.active() || s.ev.loop_ctx == Some(s.cx.cur) {
            return false;
        }
        // only the timers due a loop wake-up ago (review HU-03): natively
        // the loop thread wakes for a timer and takes its lock some time
        // after the deadline, so an extern right after the timer came due
        // (two `next`s in a row on a fresh repeating timer, `next` then
        // `reset`) acts first
        let now = Instant::now();
        s.ev_check_by(now, now.checked_sub(LOOP_LATENCY).unwrap_or(now), true);
        s.ev_start_loop();
        match s.ev.loop_ctx {
            Some(c) => c != s.cx.cur && s.cx.ctxs[c].status == Status::Runnable,
            None => false,
        }
    });
    if go {
        super::ctx::yield_now();
    }
}

/// How long after a timer's deadline the extern of another thread can still
/// take the loop's lock first (`catch_up`, review HU-03): one wake-up of the
/// loop thread from `epoll_wait`, some tens of microseconds on an idle host
/// and up to about a millisecond on a loaded one. 1 ms (review RF14-02: with
/// 100 µs, the second of two externs in a row could come after it on a
/// loaded host, in a debug build).
const LOOP_LATENCY: Duration = Duration::from_millis(1);

/// The loop's time for the timer callback that runs now on the loop context
/// (libuv's `loop->time` of the iteration that runs it, review HU-06): the
/// time of the look that found the timer due, or, for a timer a look found
/// due while the loop context ran another callback, the time the loop
/// context took it (`loop_main`). `None` outside a timer's callback. A
/// repeating timer arms its next period from it, as `uv_timer_again` does.
pub(crate) fn loop_time() -> Option<Instant> {
    with(|s| s.ev.fired_at)
}

/// Block the running context until `deadline` or until another wakes it
/// (`wake`), whichever comes first: the io layer's retries of calls that
/// cannot be polled (`flock`, a child without a pidfd).
#[cfg(feature = "io")]
pub(crate) fn block_until(deadline: Instant) {
    block(Wait::Sleep(deadline));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From the loop seeing it ready until its call returns, a watch is out
    /// of epoll's set; a queued call whose descriptor was drained meanwhile
    /// is skipped, and the watch is armed again (review RSIO-02).
    #[test]
    fn a_queued_watch_is_disarmed_and_rechecked() {
        let (r, w) = rustix::pipe::pipe().unwrap();
        let raw = r.as_raw_fd();
        let calls = Rc::new(Cell::new(0));
        let c2 = calls.clone();
        let id = watch(r, Interest::READ, Rc::new(move |_| c2.set(c2.get() + 1))).unwrap();
        let armed = |raw| with(|s| s.ev.regs.get(&raw).map(|r| r.added));
        assert_eq!(armed(raw), Some(epoll::EventFlags::IN));
        rustix::io::write(&w, b"x").unwrap();
        with(|s| s.ev_check(Instant::now(), true));
        // queued: out of the set
        assert_eq!(with(|s| s.ev.due.len()), 1);
        assert_eq!(armed(raw), Some(epoll::EventFlags::empty()));
        // another reader drains it before the call runs
        let mut b = [0u8; 4];
        let r2 = with(|s| {
            let w = s.ev.regs[&raw].watch.as_ref().unwrap();
            rustix::io::read(w.fd.as_fd(), &mut b)
        });
        assert_eq!(r2.unwrap(), 1);
        // the call is skipped, and the watch is armed again
        assert!(with(|s| s.ev.next_due()).is_none());
        assert_eq!(calls.get(), 0);
        assert_eq!(armed(raw), Some(epoll::EventFlags::IN));
        unwatch(id);
        assert!(armed(raw).is_none());
    }

    /// Review HU-04 (fixes-14): within one look the descriptors' events are
    /// queued before the timers due, as libuv runs the io callbacks before
    /// the timers in one iteration.
    #[test]
    fn within_one_look_descriptor_events_come_before_due_timers() {
        let (r, w) = rustix::pipe::pipe().unwrap();
        let id = watch(r, Interest::READ, Rc::new(|_| {})).unwrap();
        let t = timer_start(Instant::now(), Rc::new(|| {}));
        rustix::io::write(&w, b"x").unwrap();
        assert!(with(|s| s.ev_check(Instant::now(), true)));
        let fd_first: Vec<bool> = with(|s| {
            let v = s.ev.due.iter().map(|d| matches!(d, Due::Fd(..))).collect();
            s.ev.due.clear();
            v
        });
        assert_eq!(fd_first, [true, false]);
        timer_stop(t);
        unwatch(id);
    }

    /// Review HU-03 (fixes-14): the catch-up's look (`ev_check_by` with the
    /// timers due a loop wake-up ago) leaves a timer that came due less
    /// than `LOOP_LATENCY` ago, and takes one due earlier.
    #[test]
    fn a_catch_up_leaves_a_timer_due_within_the_loop_latency() {
        let d = Instant::now();
        let t = timer_start(d, Rc::new(|| {}));
        let look = |now: Instant| with(|s| s.ev_check_by(now, now - LOOP_LATENCY, true));
        assert!(!look(d + LOOP_LATENCY / 2));
        assert!(look(d + LOOP_LATENCY));
        with(|s| s.ev.due.clear());
        timer_stop(t);
    }

    #[test]
    fn ready_from_flags() {
        let r = Ready::from_poll(PollFlags::HUP);
        assert!(r.read && r.write && r.hangup && !r.error);
        assert!(r.meets(Interest::READ));
        let r = Ready::from_epoll(epoll::EventFlags::OUT);
        assert!(!r.read && r.write);
        assert!(!r.meets(Interest::READ) && r.meets(Interest::WRITE));
    }
}
