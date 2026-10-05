//! Contexts: what a thread is to native Lean, on one thread here.
//!
//! Native Lean runs tasks on a pool of worker threads, and `main` on a thread
//! of its own; a thread that blocks (a task or promise not finished yet, a
//! mutex, a condition variable, a sleep) lets the others go on. This runtime
//! runs everything on one thread, with *contexts* instead: `main`'s, on the
//! thread's own stack, and one per task the scheduler starts, each a
//! coroutine on a stack of its own (`corosensei`). A task that is needed
//! (`Task.get`, `IO.wait`) still runs right there, on the stack of whoever
//! needs it; a context switch happens only when the running context blocks,
//! or lets the others go first at an effect or polling point.
//!
//! Every switch goes through `main`'s stack: corosensei's coroutines are
//! asymmetric (a coroutine suspends to whoever resumed it), so `main`'s
//! context, when it blocks, runs the *hub* (`hub`), which resumes the
//! contexts that can go on one at a time; a context that blocks suspends back
//! to the hub. Natively the same order holds: the hub picks, in this order, a
//! context that can go on (in the order they became able to), a queued task on
//! a new context (as Lean's task manager would start it, within its number of
//! workers), else it waits for the earliest sleeper. From lean2rr's leanrt
//! (`sched.rs`, `coro.rs`), with the hand-written stack switch replaced by
//! corosensei.
//!
//! Suspending needs the running coroutine's `Yielder`, which corosensei hands
//! only to the coroutine's entry closure; the code that blocks is deep below
//! it (in the translated program). Reaching it from there means
//! dereferencing a stored pointer, which safe Rust cannot express, so that
//! one step is the translator's glue (`Glue::suspend`, `Suspend`); the
//! scheduler decides when it may happen (docs/sched.md, "The glue").

use super::task::CtxState;
use super::{glue, with, Sched};
use corosensei::stack::{DefaultStack, Stack};
use corosensei::{Coroutine, CoroutineResult};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{compiler_fence, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// corosensei's yielder of a context (input and output `()`: what the
/// scheduler passes between contexts lives in its own state).
pub type Yielder = corosensei::Yielder<(), ()>;

type Co = Coroutine<(), (), (), DefaultStack>;

/// A context of the scheduler of this thread, for the glue's waiter lists
/// (`current_context`, `wake`). Opaque: it names a context only on the
/// thread whose scheduler made it, and is reused once that context has
/// ended.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CtxId(u32);

/// `main`'s context (also the one of the initializers before it).
pub const MAIN: CtxId = CtxId(0);

impl CtxId {
    /// The context's number, for the wait cores' 4-byte records
    /// (`sched::Gate`, `src/sched/wait.rs`). `u32::MAX` is never a context.
    pub(crate) fn index(self) -> u32 {
        self.0
    }
}

impl std::ops::Index<CtxId> for Vec<Ctx> {
    type Output = Ctx;
    fn index(&self, c: CtxId) -> &Ctx {
        &self[c.0 as usize]
    }
}

impl std::ops::IndexMut<CtxId> for Vec<Ctx> {
    fn index_mut(&mut self, c: CtxId) -> &mut Ctx {
        &mut self[c.0 as usize]
    }
}

/// The translator's part of the scheduler: the one step safe Rust cannot
/// express (`suspend`), and the hooks for what a thread owns natively.
///
/// Every method is called with no borrow of the scheduler's state held, so
/// it may call the scheduler's functions, with two exceptions: `suspend`
/// must do nothing but suspend, and `switched`, which the hub runs on
/// `main`'s stack, must not call one that may block or yield (the scheduler
/// panics if it does). When nothing can run, the hub waits in the
/// scheduler's own event loop (`reactor::idle`), not in the glue.
pub trait Glue {
    /// Suspend the running context: call `(*s.yielder()).suspend(())`, and
    /// return when that returns (the hub has resumed the context).
    ///
    /// The scheduler calls this only from `switch_away`, on a context it
    /// started (never on `main`'s), while that context runs, from that
    /// context's stack, with that context's own yielder, which corosensei
    /// keeps at the base of the context's stack until the context's function
    /// returns. The pointer is therefore valid during the call. The glue
    /// dereferences it here and nowhere else, and does not keep or copy it.
    /// This is the glue's only `unsafe` step; the proof is in docs/sched.md
    /// ("Why `Glue::suspend` is sound").
    fn suspend(&self, s: Suspend<'_>);

    /// The running context changes from `from` to `to` (every switch goes
    /// through `main`'s context, so one of them is `MAIN`): for the glue's
    /// own per-thread state, which natively each thread has. The io layer's
    /// (the current standard streams, the modelled `errno`) the scheduler
    /// swaps itself, with the feature `io` (`slots`, review AR-24): the glue
    /// must not swap `io::streams` too. Runs on `main`'s stack, before `to`
    /// runs or after `from` has stopped: it must not block or yield. A panic
    /// in it aborts the process.
    fn switched(&self, _from: CtxId, _to: CtxId) {}

    /// A task starts running. `own_thread`: natively on a thread of its own
    /// (a worker's pool task, a dedicated task); otherwise on the current
    /// thread (a `sync` dependent, a task at priority `LEAN_SYNC_PRIO`),
    /// sharing its state. For the glue's own per-task state: the io layer's
    /// streams and `errno` are the scheduler's (`slots`: a pool task gets
    /// the set of the emulated worker it occupies, which keeps what the
    /// task leaves, as natively; review AR-24).
    fn task_begin(&self, _own_thread: bool) {}

    /// The task started by the matching `task_begin` has finished (its
    /// `sync` dependents have run), or waits for the task its bind function
    /// returned, or a Rust panic unwinds its run. In the last case it runs
    /// during the unwinding, where a panic aborts the process.
    fn task_end(&self, _own_thread: bool) {}

    /// The task manager's finalization ends its standard workers (review
    /// AR-34): in `finish`, once no pool task is queued or running, before
    /// the dedicated tasks are waited for and before `main`'s streams are
    /// flushed, as natively `~task_manager` joins the standard workers, whose
    /// thread finalizers drop their thread-local state, before it waits for
    /// the dedicated threads. The scheduler drops the io layer's
    /// per-worker sets (`slots`) right before; a glue with per-worker state
    /// of its own (kept by `running_worker`, lean2rr's stream cells) drops
    /// it here. Called once. A pool task that begins later (a dedicated
    /// task's dependent, LB-13's corrected run) starts with a fresh set,
    /// dropped at its end: the glue does the same with its own (fresh at
    /// that task's `task_begin`, dropped at its `task_end`).
    fn workers_end(&self) {}
}

/// The running context's yielder, handed to `Glue::suspend`. Only the
/// scheduler makes one, for the context that runs, during the call. The
/// lifetime is documentary (nothing ties it to the call): not keeping the
/// value or its pointer is the glue's duty (docs/sched.md, S3).
pub struct Suspend<'a> {
    y: *const Yielder,
    _call: std::marker::PhantomData<&'a ()>,
}

impl Suspend<'_> {
    /// The yielder to suspend with (see `Glue::suspend`).
    pub fn yielder(&self) -> *const Yielder {
        self.y
    }
}

/// What a blocked context waits for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Wait {
    None,
    /// The task or promise with this entry and generation finishes.
    Cell(u32, u32),
    /// Something changed that a waiting walk looks at: a task's finish
    /// notified, a task was queued or started, a context ended
    /// (`source_wait`: a dependent the walk of its source has not queued
    /// yet). Natively a `wait_for`: a pool waiter's worker is free meanwhile.
    Progress,
    /// `IO.waitAny`: woken as `Progress` is (`wait_any` tells a notification
    /// from the rest by `notify_seq`), but the context keeps its worker, as
    /// native `wait_any` does not raise the worker limit (AR-10 (ii)).
    Any,
    /// `main` has returned and waits for the remaining tasks: woken when a
    /// context ends, a task finishes or is queued.
    FinalRun,
    /// Whoever hands it a synchronization object or a value wakes it
    /// (`sync`, the glue's thunks).
    Sync,
    /// A sleep until the deadline.
    Sleep(Instant),
    /// Descriptors registered with the loop (`reactor::poll_fds`), until one is
    /// ready or the deadline.
    Io(Option<Instant>),
    /// Nothing: a context that waits forever (a thunk forced inside its own
    /// computation, `Promise.result!` on a dropped promise). It keeps its
    /// worker, as natively that thread spins or sleeps forever.
    Forever,
    /// Nothing either: a `wait` that can never end, for the running task
    /// itself, a dependent of it, or the walk of its own dependents (review
    /// AR-15). Natively a `wait_for`, which raises the worker limit by one
    /// for a pool task, so the context does not hold its worker.
    OnItself,
}

impl Wait {
    /// When a sleeper's wait ends by itself.
    pub(crate) fn deadline(self) -> Option<Instant> {
        match self {
            Wait::Sleep(d) | Wait::Io(Some(d)) => Some(d),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    Running,
    Runnable,
    Blocked,
    Dead,
}

/// The stack of a context, for a stack-overflow report: its guard page(s)
/// `[guard_lo, guard_hi)` and its top; the usable part is
/// `[guard_hi, top)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StackBounds {
    pub guard_lo: usize,
    pub guard_hi: usize,
    pub top: usize,
}

thread_local! {
    /// The stack of the context running on this thread (`running_stack`);
    /// the hub sets these at every switch (`publish`). All zero while
    /// `main`'s context runs (on the thread's own stack). Lean's
    /// stack-overflow report does not read them: its handler keeps a record
    /// of its own (`stack_overflow::publish`), since a thread-local is not
    /// async-signal-safe in every link mode.
    static RUN_LO: AtomicUsize = const { AtomicUsize::new(0) };
    static RUN_HI: AtomicUsize = const { AtomicUsize::new(0) };
    static RUN_TOP: AtomicUsize = const { AtomicUsize::new(0) };
}

/// The stack of the context running on the calling thread, `None` on
/// `main`'s context (the thread's own stack).
///
/// Not for a signal handler: Lean's stack-overflow report is the crate's
/// (`install_stack_overflow_handler`, docs/sched.md, item 8 of "The glue"),
/// and reads a record of its own, without thread-locals.
pub fn running_stack() -> Option<StackBounds> {
    let lo = RUN_LO.with(|x| x.load(Ordering::Relaxed));
    if lo == 0 {
        return None;
    }
    Some(StackBounds {
        guard_lo: lo,
        guard_hi: RUN_HI.with(|x| x.load(Ordering::Relaxed)),
        top: RUN_TOP.with(|x| x.load(Ordering::Relaxed)),
    })
}

/// The context running on this thread changes: the hub calls it right
/// before it resumes a context (`Some`, its stack) and right after the
/// context is back (`None`), and `PanicGuard` after a panic. It updates
/// `running_stack()` and, with the feature `stack-overflow`, the record of
/// Lean's stack-overflow report (its proof, A3 in docs/native-quirks.md,
/// relies on these two call sites).
fn publish(b: Option<StackBounds>) {
    #[cfg(feature = "stack-overflow")]
    super::stack_overflow::publish(b);
    let b = b.unwrap_or(StackBounds {
        guard_lo: 0,
        guard_hi: 0,
        top: 0,
    });
    // `guard_lo` last (0 first), so a nonzero `guard_lo` comes with its own
    // bounds.
    RUN_LO.with(|x| x.store(0, Ordering::Relaxed));
    compiler_fence(Ordering::SeqCst);
    RUN_HI.with(|x| x.store(b.guard_hi, Ordering::Relaxed));
    RUN_TOP.with(|x| x.store(b.top, Ordering::Relaxed));
    compiler_fence(Ordering::SeqCst);
    RUN_LO.with(|x| x.store(b.guard_lo, Ordering::Relaxed));
}

pub(crate) struct Ctx {
    pub(crate) status: Status,
    pub(crate) wait: Wait,
    /// The coroutine, while suspended (the hub takes it out to resume it);
    /// `None` for `main`'s context.
    co: Option<Co>,
    /// Its yielder, set by its entry; stays valid until its function returns.
    yielder: *const Yielder,
    bounds: Option<StackBounds>,
    /// The usable size of its stack (`Contexts::stack_size` when it started).
    stack_size: usize,
    /// The thread number of tasks running on it outside of any other task
    /// (`thread_number`): 0 for `main`'s.
    pub(crate) thread_base: u64,
    /// Its task bookkeeping (running tasks, walks of dependents), as a
    /// thread's.
    pub(crate) st: CtxState,
    /// A worker's first task (entry, generation).
    pub(crate) preselect: Option<(u32, u32)>,
    /// When it last became able to run (`effect`).
    pub(crate) ready: Instant,
    /// It lets the others go first at an effect point (`effect`).
    pub(crate) at_effect: bool,
    /// It holds one of the task manager's workers, as last counted in
    /// `Contexts::in_use` (`Sched::refresh_holds`).
    pub(crate) holds: bool,
    /// The emulated pool worker of the innermost task running on it
    /// (`running_worker`, review AR-32): `None` for a dedicated or a `sync`
    /// task, and outside tasks.
    pub(crate) worker: Option<u32>,
}

impl Ctx {
    fn new(thread_base: u64) -> Ctx {
        Ctx {
            status: Status::Runnable,
            wait: Wait::None,
            co: None,
            yielder: std::ptr::null(),
            bounds: None,
            stack_size: 0,
            thread_base,
            st: CtxState::default(),
            preselect: None,
            ready: Instant::now(),
            at_effect: false,
            holds: false,
            worker: None,
        }
    }
}

pub(crate) struct Contexts {
    pub(crate) ctxs: Vec<Ctx>,
    free: Vec<CtxId>,
    pub(crate) cur: CtxId,
    /// Contexts that can go on, in the order they became able to.
    pub(crate) runnable: VecDeque<CtxId>,
    /// Sleeping contexts and their deadlines.
    pub(crate) sleepers: Vec<(Instant, CtxId)>,
    /// Contexts waiting for a task or promise (entry, generation).
    cell_waiters: HashMap<(u32, u32), Vec<CtxId>>,
    /// Contexts waiting for a task to finish or be queued (`Wait::Progress`,
    /// `Wait::Any`, `Wait::FinalRun`).
    progress_waiters: Vec<CtxId>,
    pub(crate) blocked: u32,
    /// Live worker contexts.
    pub(crate) workers: u32,
    /// The number of the task manager's workers (`LEAN_NUM_THREADS`).
    pub(crate) pool_limit: u32,
    /// Stacks of ended contexts, for new ones.
    stacks: Vec<DefaultStack>,
    stack_size: usize,
    /// A context lets the others go first at an effect point (`effect`).
    pub(crate) in_effect: bool,
    pub(crate) glue: Option<Rc<dyn Glue>>,
    /// The contexts holding one of the task manager's workers
    /// (`Ctx::holds`; `Sched::pool_in_use`).
    pub(crate) in_use: u32,
}

/// The next worker context's number, process-wide: unique across the
/// schedulers of several threads. A context's thread numbers are this
/// number times 2^32, plus the depth of nested tasks on it (`begin`), so
/// they neither wrap nor run into the next context's (review RS1S-07).
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);

/// How many stacks of ended contexts are kept for new ones. Their touched
/// pages stay resident (as a native worker thread's stack does).
const POOLED_STACKS: usize = 8;

/// Linux's `ENOMEM` and `EAGAIN` (the same on aarch64 and x86-64; `sched`
/// may be built without `io`, whose constants these are).
const ENOMEM: i32 = 12;
const EAGAIN: i32 = 11;

impl Contexts {
    pub(crate) fn new() -> Contexts {
        let mut main = Ctx::new(0);
        main.status = Status::Running;
        Contexts {
            ctxs: vec![main],
            free: Vec::new(),
            cur: MAIN,
            runnable: VecDeque::new(),
            sleepers: Vec::new(),
            cell_waiters: HashMap::new(),
            progress_waiters: Vec::new(),
            blocked: 0,
            workers: 0,
            pool_limit: 0,
            stacks: Vec::new(),
            stack_size: 1 << 30,
            in_effect: false,
            glue: None,
            in_use: 0,
        }
    }

    pub(crate) fn set_stack_size(&mut self, size: usize) {
        // A multiple of 64 KiB (the largest page size of Linux targets), so
        // that the guard's size can be read off a stack (`bounds_of`).
        self.stack_size = size.max(1 << 16).div_ceil(1 << 16).saturating_mul(1 << 16);
        self.stacks.clear();
    }

    /// The number of live contexts, `main`'s included.
    pub(crate) fn live(&self) -> usize {
        self.ctxs.len() - self.free.len()
    }

    pub(crate) fn cur_ctx(&mut self) -> &mut Ctx {
        &mut self.ctxs[self.cur]
    }
}

impl Drop for Contexts {
    fn drop(&mut self) {
        // At thread exit, suspended contexts are not unwound: their stacks
        // hold Lean values whose destructors would call back into the
        // scheduler during its destruction. Natively the process exits with
        // its threads where they are.
        for c in self.ctxs.drain(..) {
            if let Some(co) = c.co {
                std::mem::forget(co);
            }
        }
    }
}

/// The guard and top of a stack of `size` usable bytes (a multiple of the
/// page size): corosensei maps `size` plus one guard page below it.
fn bounds_of(st: &DefaultStack, size: usize) -> StackBounds {
    let top = st.base().get();
    let lo = st.limit().get();
    let guard = (top - lo).saturating_sub(size);
    StackBounds {
        guard_lo: lo,
        guard_hi: lo + guard,
        top,
    }
}

impl Sched {
    pub(crate) fn wake(&mut self, c: CtxId) {
        let x = &mut self.cx.ctxs[c];
        if x.status == Status::Blocked {
            x.status = Status::Runnable;
            x.ready = Instant::now();
            x.wait = Wait::None;
            self.cx.blocked -= 1;
            self.cx.runnable.push_back(c);
            self.refresh_holds(c);
        }
    }

    /// Wake context `c` if it waits in `block_sync` (`Wait::Sync`): the
    /// wake of the glue's own waiter lists and of the wait cores, so that a
    /// stale entry (a context that went on, after a panic, to another kind
    /// of wait) never cuts that other wait short (review RW1-08).
    pub(crate) fn wake_sync(&mut self, c: CtxId) {
        let x = &self.cx.ctxs[c];
        if x.status == Status::Blocked && x.wait == Wait::Sync {
            self.wake(c);
        }
    }

    /// Wake context `c` if it naps in `block_until` (`Wait::Sleep`): an io
    /// wait that looks again every few milliseconds (a contended `flock`),
    /// cut short by the event it waits for (an unlock in this process). Its
    /// stale sleeper entry is dropped by `promote_sleepers` (review NEW-1
    /// of wait-1: `sched::wake` wakes `Wait::Sync` only).
    #[cfg(feature = "io")]
    pub(crate) fn wake_napping(&mut self, c: CtxId) {
        let x = &self.cx.ctxs[c];
        if x.status == Status::Blocked && matches!(x.wait, Wait::Sleep(_)) {
            self.wake(c);
        }
    }

    /// Whether context `c` is able to run (runnable or running).
    #[cfg(feature = "io")]
    pub(crate) fn can_run(&self, c: CtxId) -> bool {
        matches!(self.cx.ctxs[c].status, Status::Runnable | Status::Running)
    }

    /// Wake the contexts blocked in `wait` on the task or promise (entry,
    /// generation), which has its value (part of a notification,
    /// `notify_all`).
    pub(crate) fn wake_cell(&mut self, key: (u32, u32)) {
        if let Some(ws) = self.cx.cell_waiters.remove(&key) {
            for c in ws {
                if self.cx.ctxs[c].wait == Wait::Cell(key.0, key.1) {
                    self.wake(c);
                }
            }
        }
    }

    /// Whether some context waits for a particular task (`Wait::Cell`).
    pub(crate) fn has_cell_waiters(&self) -> bool {
        !self.cx.cell_waiters.is_empty()
    }

    /// Whether a context blocked in `wait` waits for a task or promise
    /// (entry, generation) for which `f` holds.
    pub(crate) fn some_cell_waiter(&self, f: impl Fn(u32, u32) -> bool) -> bool {
        self.cx.cell_waiters.iter().any(|(&(i, g), ws)| {
            f(i, g)
                && ws.iter().any(|&c| {
                    let x = &self.cx.ctxs[c];
                    x.status == Status::Blocked && x.wait == Wait::Cell(i, g)
                })
        })
    }

    /// Something changed that `Wait::Progress`, `Wait::Any` and
    /// `Wait::FinalRun` waiters look at: a task's finish notified, a task was
    /// queued or started, a context ended.
    pub(crate) fn wake_progress(&mut self) {
        if self.cx.progress_waiters.is_empty() {
            return;
        }
        for c in std::mem::take(&mut self.cx.progress_waiters) {
            if matches!(
                self.cx.ctxs[c].wait,
                Wait::Progress | Wait::Any | Wait::FinalRun
            ) {
                self.wake(c);
            }
        }
    }

    /// Register the running context as blocked on `w`.
    fn register_block(&mut self, w: Wait) {
        let c = self.cx.cur;
        let x = &mut self.cx.ctxs[c];
        debug_assert_eq!(x.status, Status::Running);
        x.status = Status::Blocked;
        x.wait = w;
        self.cx.blocked += 1;
        self.refresh_holds(c);
        match w {
            Wait::Cell(i, g) => self.cx.cell_waiters.entry((i, g)).or_default().push(c),
            Wait::Progress | Wait::Any | Wait::FinalRun => self.cx.progress_waiters.push(c),
            Wait::Sleep(d) | Wait::Io(Some(d)) => self.cx.sleepers.push((d, c)),
            _ => {}
        }
    }

    /// Wake the sleepers whose deadline has passed (in deadline order, as
    /// their threads would wake); the earliest deadline left.
    pub(crate) fn promote_sleepers(&mut self, now: Instant) -> Option<Instant> {
        if self.cx.sleepers.is_empty() {
            return None;
        }
        let mut next: Option<Instant> = None;
        let mut due: Vec<(Instant, CtxId)> = Vec::new();
        let mut i = 0;
        while i < self.cx.sleepers.len() {
            let (d, c) = self.cx.sleepers[i];
            let x = &self.cx.ctxs[c];
            if x.status != Status::Blocked || x.wait.deadline() != Some(d) {
                self.cx.sleepers.swap_remove(i);
                continue;
            }
            if d <= now {
                self.cx.sleepers.swap_remove(i);
                due.push((d, c));
                continue;
            }
            next = Some(next.map_or(d, |n| n.min(d)));
            i += 1;
        }
        due.sort();
        for (_, c) in due {
            self.wake(c);
        }
        next
    }

    /// Whether a sleeper's deadline has passed (for effect points).
    pub(crate) fn sleeper_due(&self, now: Instant) -> bool {
        self.cx
            .sleepers
            .iter()
            .any(|&(d, c)| d <= now && self.cx.ctxs[c].wait.deadline() == Some(d))
    }

    /// Start entry `e` (generation `g`) on a new worker context, able to run.
    pub(crate) fn start_worker(&mut self, e: u32, g: u32) -> CtxId {
        let id = self.start_context(worker_main);
        self.cx.ctxs[id].preselect = Some((e, g));
        id
    }

    /// A new context running `entry` (a worker's, or the event loop's),
    /// able to run.
    pub(crate) fn start_context(&mut self, entry: fn()) -> CtxId {
        let size = self.cx.stack_size;
        let stack = match self.cx.stacks.pop() {
            Some(st) => st,
            None => match DefaultStack::new(size) {
                Ok(st) => st,
                // natively a thread that cannot map its stack: glibc's
                // `pthread_create` reports the mapping's error, with
                // `ENOMEM` turned into `EAGAIN`
                Err(e) => {
                    let code = match e.raw_os_error() {
                        Some(ENOMEM) | None => EAGAIN,
                        Some(c) => c,
                    };
                    super::thread_create_failed(&std::io::Error::from_raw_os_error(code))
                }
            },
        };
        let bounds = bounds_of(&stack, size);
        let id = match self.cx.free.pop() {
            Some(i) => i,
            None => {
                self.cx.ctxs.push(Ctx::new(0));
                CtxId((self.cx.ctxs.len() - 1) as u32)
            }
        };
        let co = Co::with_stack(stack, move |y: &Yielder, ()| {
            // The yielder sits at the base of this stack until this function
            // returns; the scheduler hands it to `Glue::suspend` while this
            // context runs.
            let p: *const Yielder = y;
            with(|s| s.cx.ctxs[id].yielder = p);
            entry();
            // R6 when a context ends: the deferred resolutions it left (a
            // drain whose end was not reported) run here, on it; debug
            // builds report them (review RW1-01)
            super::drain::context_ends();
            with(|s| s.die());
        });
        let tb = NEXT_THREAD.fetch_add(1, Ordering::Relaxed) << 32;
        let mut x = Ctx::new(tb);
        x.co = Some(co);
        x.bounds = Some(bounds);
        x.stack_size = size;
        self.cx.ctxs[id] = x;
        self.cx.workers += 1;
        self.cx.runnable.push_back(id);
        id
    }

    /// The running worker context has nothing left to do: it ends.
    fn die(&mut self) {
        let c = self.cx.cur;
        let x = &mut self.cx.ctxs[c];
        x.status = Status::Dead;
        x.wait = Wait::None;
        self.cx.workers -= 1;
        self.refresh_holds(c);
        self.wake_progress();
    }

    /// The hub's next step (lean2rr's `schedule`).
    fn hub_step(&mut self) -> HubStep {
        loop {
            let now = Instant::now();
            let next_deadline = self.promote_sleepers(now);
            // The event loop: due timers, ready descriptors (sched-io).
            self.ev_check(now, false);
            self.ev_start_loop();
            if let Some(n) = self.cx.runnable.pop_front() {
                if self.cx.ctxs[n].status != Status::Runnable {
                    continue;
                }
                self.cx.ctxs[n].status = Status::Running;
                if n == MAIN {
                    self.cx.cur = MAIN;
                    return HubStep::Main;
                }
                // `cur` becomes `n`, and the coroutine leaves the table, only
                // right before it is resumed (`hub`).
                return HubStep::Resume(n, self.cx.ctxs[n].bounds);
            }
            // A queued task on a new worker context.
            if let Some((e, g)) = self.startable(super::task::Gate::Any) {
                self.start_worker(e, g);
                continue;
            }
            // `startable` marked started a pure task that a context waits
            // for, which woke it (`pick`): it runs before the hub waits.
            if !self.cx.runnable.is_empty() {
                continue;
            }
            // A context waits for a queued pure task that only the started
            // pure tasks keep from starting: the oldest runs, as natively
            // its worker finishes it (review AR-25).
            if let Some((e, g)) = self.needed_picked() {
                self.start_worker(e, g);
                continue;
            }
            let next_deadline = match (next_deadline, self.ev.next_timer()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            // Nothing will ever wake up by itself (no sleeper, timer or
            // registered descriptor): run what tasks a worker has started
            // without running them yet (pure ones, `task::pick`).
            if next_deadline.is_none() && !self.ev.has_regs() {
                if let Some((e, g)) = self.last_resort() {
                    self.start_worker(e, g);
                    continue;
                }
            }
            return HubStep::Idle(next_deadline);
        }
    }

    /// Context `n` is about to be resumed: it becomes the running context
    /// (S2 in docs/sched.md), and its coroutine leaves the table.
    fn enter(&mut self, n: CtxId) -> Co {
        self.cx.cur = n;
        self.cx.ctxs[n]
            .co
            .take()
            .expect("lean-runtime: a context able to run has no coroutine")
    }

    /// Control is back on `main`'s stack after context `n` suspended or
    /// ended. A suspended coroutine goes back into the table as the last
    /// step, so that it is held by `Parked` until then (S5 in
    /// docs/sched.md).
    fn after_resume(&mut self, n: CtxId, co: &mut Option<Co>, ended: bool) {
        self.cx.cur = MAIN;
        if ended {
            debug_assert_eq!(self.cx.ctxs[n].status, Status::Dead);
            // Completed: taking it out of `Parked` is safe from here on.
            let st = co
                .take()
                .expect("lean-runtime: no coroutine to put back")
                .into_stack();
            // Only a stack of the current size is reused (`start_with` may
            // have changed it since this context started).
            if self.cx.stacks.len() < POOLED_STACKS
                && self.cx.ctxs[n].stack_size == self.cx.stack_size
            {
                self.cx.stacks.push(st);
            }
            let x = &mut self.cx.ctxs[n];
            x.yielder = std::ptr::null();
            x.bounds = None;
            self.cx.free.push(n);
        } else {
            let slot = &mut self.cx.ctxs[n].co;
            debug_assert!(slot.is_none());
            *slot = co.take();
        }
    }
}

enum HubStep {
    Main,
    Resume(CtxId, Option<StackBounds>),
    Idle(Option<Instant>),
}

thread_local! {
    /// The hub is running a glue hook (`switched`): no context may
    /// block or yield now (`switch_away`), since the code runs on `main`'s
    /// stack whatever context is about to run (docs/sched.md, S4).
    static IN_HUB_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// The two ways to leave a context, `block` and `yield_now`, start here.
fn not_in_hub_hook() {
    assert!(
        !IN_HUB_HOOK.with(Cell::get),
        "lean-runtime: a glue hook called by the hub (`switched`) must not block or yield"
    );
}

/// Run a glue hook from the hub (see `IN_HUB_HOOK`). A panic in the hook
/// aborts the process, after Rust's message: unwinding the hub with a
/// context half switched would leave `main` blocked for good (review
/// RS1S-12 of sched-1).
fn hub_hook(f: impl FnOnce()) {
    struct Abort;
    impl Drop for Abort {
        fn drop(&mut self) {
            use std::io::Write;
            let _ = std::io::stderr().write_all(
                b"lean-runtime: a panic in a glue hook run by the hub (`switched`); aborting\n",
            );
            std::process::abort();
        }
    }
    IN_HUB_HOOK.with(|h| h.set(true));
    let abort = Abort;
    f();
    std::mem::forget(abort);
    IN_HUB_HOOK.with(|h| h.set(false));
}

/// On `main`'s context: run other contexts until `main`'s is able to go on
/// and its turn comes.
fn hub() {
    loop {
        match with(|s| s.hub_step()) {
            HubStep::Main => return,
            HubStep::Resume(n, bounds) => {
                let g = glue();
                // the context's standard streams and `errno` in, `main`'s
                // aside (`slots`, review AR-24); back after it, also when a
                // panic unwinds from it
                #[cfg(feature = "io")]
                let slots =
                    super::slots::ContextSlots::enter(n.0 as usize, with(|s| s.is_loop_ctx(n)));
                hub_hook(|| g.switched(MAIN, n));
                // From here until `resume` returns, `n` is the running
                // context (S2). A Rust panic in it unwinds to its base, and
                // corosensei resumes it here, on `main`'s stack: the guard
                // then marks the context dead and `main` current again before
                // the unwinding goes on (S6).
                let guard = PanicGuard(n);
                let mut co = Parked(Some(with(|s| s.enter(n))));
                publish(bounds);
                let r = co.0.as_mut().map(|c| c.resume(()));
                std::mem::forget(guard);
                publish(None);
                let ended = matches!(r, Some(CoroutineResult::Return(())));
                with(|s| s.after_resume(n, &mut co.0, ended));
                hub_hook(|| g.switched(n, MAIN));
                #[cfg(feature = "io")]
                slots.leave(ended);
            }
            HubStep::Idle(d) => super::reactor::idle(d),
        }
    }
}

/// A coroutine out of the context table, between `Sched::enter` and the end
/// of `Sched::after_resume` (S5 in docs/sched.md). That stretch must not
/// panic; if a panic ever unwound it anyway, a coroutine still suspended is
/// forgotten here, never dropped: dropping it would unwind its stack
/// (corosensei's forced unwind), making a pending `Yielder::suspend` return
/// by unwinding. A completed or unstarted one is dropped as usual.
struct Parked(Option<Co>);

impl Drop for Parked {
    fn drop(&mut self) {
        if let Some(co) = self.0.take() {
            if co.started() && !co.done() {
                std::mem::forget(co);
            }
        }
    }
}

struct PanicGuard(CtxId);

impl Drop for PanicGuard {
    fn drop(&mut self) {
        publish(None);
        let n = self.0;
        with(|s| {
            s.cx.cur = MAIN;
            let x = &mut s.cx.ctxs[n];
            if x.status != Status::Dead {
                x.status = Status::Dead;
                x.wait = Wait::None;
                s.cx.workers -= 1;
            }
            s.refresh_holds(n);
            // The context's slot is not reused: its coroutine went with the
            // panic.
            //
            // The panic goes on as `main`'s, which was blocked (or let others
            // go first) in the hub: it runs again, waits for nothing, and an
            // effect round it was in is over (review RS1S-04). Its entries in
            // the waiter lists are stale now, and are skipped.
            let m = &mut s.cx.ctxs[MAIN];
            if m.status == Status::Blocked {
                s.cx.blocked -= 1;
            }
            m.status = Status::Running;
            m.wait = Wait::None;
            m.at_effect = false;
            s.cx.in_effect = false;
            s.refresh_holds(MAIN);
        });
    }
}

/// Block the running context until it is woken for `w`; other contexts run
/// meanwhile. Returns once it runs again.
pub(crate) fn block(w: Wait) {
    not_in_hub_hook();
    let (cur, y) = with(|s| {
        s.register_block(w);
        (s.cx.cur, s.cx.cur_ctx().yielder)
    });
    switch_away(cur, y);
}

/// Let other contexts that can go on run first (the running one goes on
/// after them).
pub(crate) fn yield_now() {
    not_in_hub_hook();
    let (cur, y) = with(|s| {
        let c = s.cx.cur;
        let x = &mut s.cx.ctxs[c];
        x.status = Status::Runnable;
        x.ready = Instant::now();
        s.cx.runnable.push_back(c);
        (c, x.yielder)
    });
    switch_away(cur, y);
}

/// Let the hub run other contexts: on `main`'s context, run it; on another
/// one, suspend through the glue with that context's own yielder `y` (S3).
fn switch_away(cur: CtxId, y: *const Yielder) {
    // R6 of the deferred resolutions (`drain`): none is queued at a switch,
    // since every drain ends with `run_deferred` before its context can
    // block, and a walk moves its entries out first. A test that catches a
    // panic out of a drain and then switches makes this fire (accepted);
    // so does a drain whose end the glue did not report (review RW1-01).
    debug_assert!(
        super::drain::queue_empty(),
        "lean-runtime: deferred promise resolutions queued at a context switch (R6)"
    );
    // The io layer's stream locks the context holds are recorded as held by
    // a suspended context while others run, whatever the reason of the
    // switch, so that another context that wants one waits for it (review
    // RSIO-01); they are the running context's again when it goes on (also
    // when a panic unwinds through here).
    #[cfg(feature = "io")]
    let _held = crate::io::coop::park(cur);
    // So is its no-suspend depth: a context that waits inside a scope does
    // not put the others in it (review RSIO-10); and its drain depth, which
    // goes with that scope (`DrainScope`, review RW1-02).
    let _depth = super::reactor::park_no_suspend();
    let _drain = super::drain::park_depth();
    if cur == MAIN {
        hub();
    } else {
        assert!(!y.is_null(), "lean-runtime: a context without its yielder");
        glue().suspend(Suspend {
            y,
            _call: std::marker::PhantomData,
        });
    }
}

/// A worker context's function (lean2rr's `worker_entry`): its first task,
/// then, while no other context can go on, the next queued task.
fn worker_main() {
    loop {
        if let Some(i) = with(|s| s.take_preselect()) {
            super::task::run_task(i);
        }
        let more = with(|s| {
            if !s.cx.runnable.is_empty() {
                return false;
            }
            match s.startable(super::task::Gate::Any) {
                Some(e) => {
                    s.cx.cur_ctx().preselect = Some(e);
                    true
                }
                None => false,
            }
        });
        if !more {
            break;
        }
    }
}

/// How long a context able to run, or a task a worker has picked, waits
/// before an output of the running context lets it go first: natively it
/// runs meanwhile on its own thread, and would by then have got past
/// anything that takes no time (thread wake-ups take microseconds).
pub(crate) const STALE: Duration = Duration::from_millis(5);
