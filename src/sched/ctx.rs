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
/// must do nothing but suspend, and `switched` and `idle`, which the hub
/// runs on `main`'s stack, must not call one that may block or yield (the
/// scheduler panics if they do).
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
    /// through `main`'s context, so one of them is `MAIN`). Natively each
    /// thread has its own current standard streams (`IO.setStdout` & co.);
    /// the glue saves `from`'s and installs `to`'s. Runs on `main`'s stack,
    /// before `to` runs or after `from` has stopped: it must not block or
    /// yield. A panic in it aborts the process.
    fn switched(&self, _from: CtxId, _to: CtxId) {}

    /// A task starts running. `own_thread`: natively on a worker thread, so
    /// with the process's standard streams; otherwise on the current thread
    /// (a `sync` dependent, a task at priority `LEAN_SYNC_PRIO`), sharing its
    /// streams.
    fn task_begin(&self, _own_thread: bool) {}

    /// The task started by the matching `task_begin` has finished (its
    /// `sync` dependents have run), or waits for the task its bind function
    /// returned, or a Rust panic unwinds its run. In the last case it runs
    /// during the unwinding, where a panic aborts the process.
    fn task_end(&self, _own_thread: bool) {}

    /// Nothing can go on until `deadline` (a sleeper's), or ever (`None`):
    /// wait. The scheduler looks again when this returns (an event loop may
    /// return early, having made a context able to go on). By default, a
    /// sleep, or a wait forever as a deadlocked native program does. Runs on
    /// `main`'s stack, in the hub: it must not block or yield. A panic in it
    /// aborts the process.
    fn idle(&self, deadline: Option<Instant>) {
        match deadline {
            Some(d) => std::thread::sleep(d.saturating_duration_since(Instant::now())),
            None => super::hang_thread(),
        }
    }
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
    /// Any task finishes (`IO.waitAny` when every task is running).
    Progress,
    /// `main` has returned and waits for the remaining tasks: woken when a
    /// context ends, a task finishes or is queued.
    FinalRun,
    /// Whoever hands it a synchronization object or a value wakes it
    /// (`sync`, the glue's thunks).
    Sync,
    /// A sleep until the deadline.
    Sleep(Instant),
    /// Nothing: a context that waits forever.
    Forever,
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
    /// The stack of the context running on this thread, for the glue's
    /// SIGSEGV handler, which runs on the faulting thread and must not take
    /// locks or allocate. Const-initialized thread-locals without a
    /// destructor are plain thread-local loads, safe in a signal handler.
    /// The contexts of a scheduler run on its thread, so a fault in a
    /// context's stack comes from the one running there; the hub sets these
    /// at every switch. All zero while `main`'s context runs (on the
    /// thread's own stack, which the glue knows).
    static RUN_LO: AtomicUsize = const { AtomicUsize::new(0) };
    static RUN_HI: AtomicUsize = const { AtomicUsize::new(0) };
    static RUN_TOP: AtomicUsize = const { AtomicUsize::new(0) };
}

/// The stack of the context running on the calling thread, `None` on
/// `main`'s context (the thread's own stack). Async-signal-safe.
///
/// Lean's handler (`src/runtime/stack_overflow.cpp`) reports
/// `\nStack overflow detected. Aborting.\n` and aborts when the faulting
/// address lies in the guard page below the faulting thread's stack; a glue
/// reproduces it for contexts with these bounds (docs/sched.md, "Stack
/// overflow").
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

fn publish(b: Option<StackBounds>) {
    let b = b.unwrap_or(StackBounds {
        guard_lo: 0,
        guard_hi: 0,
        top: 0,
    });
    // The order matters for a handler interrupting this on the same thread:
    // `guard_lo` last (0 first), so a nonzero `guard_lo` comes with its own
    // bounds. The fences keep the compiler from reordering the stores.
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
    /// Contexts waiting for any task to finish (`Wait::Progress`,
    /// `Wait::FinalRun`).
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

    /// The task or promise (entry, generation) has finished: wake whoever
    /// waits for it or for any task.
    pub(crate) fn on_finish(&mut self, key: (u32, u32)) {
        if self.cx.blocked == 0 {
            return;
        }
        if let Some(ws) = self.cx.cell_waiters.remove(&key) {
            for c in ws {
                if self.cx.ctxs[c].wait == Wait::Cell(key.0, key.1) {
                    self.wake(c);
                }
            }
        }
        self.wake_progress();
    }

    /// Something changed that `Wait::Progress`/`Wait::FinalRun` waiters look
    /// at: a task finished or was queued, a context ended.
    pub(crate) fn wake_progress(&mut self) {
        if self.cx.progress_waiters.is_empty() {
            return;
        }
        for c in std::mem::take(&mut self.cx.progress_waiters) {
            if matches!(self.cx.ctxs[c].wait, Wait::Progress | Wait::FinalRun) {
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
            Wait::Progress | Wait::FinalRun => self.cx.progress_waiters.push(c),
            Wait::Sleep(d) => self.cx.sleepers.push((d, c)),
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
            if x.status != Status::Blocked || x.wait != Wait::Sleep(d) {
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
            .any(|&(d, c)| d <= now && self.cx.ctxs[c].wait == Wait::Sleep(d))
    }

    /// Start entry `e` (generation `g`) on a new worker context, able to run.
    pub(crate) fn start_worker(&mut self, e: u32, g: u32) -> CtxId {
        let size = self.cx.stack_size;
        let stack = match self.cx.stacks.pop() {
            Some(st) => st,
            None => match DefaultStack::new(size) {
                Ok(st) => st,
                Err(_) => thread_create_failed(),
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
            worker_main();
        });
        let tb = NEXT_THREAD.fetch_add(1, Ordering::Relaxed) << 32;
        let mut x = Ctx::new(tb);
        x.co = Some(co);
        x.bounds = Some(bounds);
        x.stack_size = size;
        x.preselect = Some((e, g));
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
            let next_deadline = self.promote_sleepers(Instant::now());
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
            // Nothing will ever wake up by itself: run what tasks a worker has
            // started without running them yet (pure ones, `task::pick`).
            if next_deadline.is_none() {
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
    /// The hub is running a glue hook (`switched`, `idle`): no context may
    /// block or yield now (`switch_away`), since the code runs on `main`'s
    /// stack whatever context is about to run (docs/sched.md, S4).
    static IN_HUB_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// The two ways to leave a context, `block` and `yield_now`, start here.
fn not_in_hub_hook() {
    assert!(
        !IN_HUB_HOOK.with(Cell::get),
        "lean-runtime: a glue hook called by the hub (`switched`, `idle`) must not block or yield"
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
                b"lean-runtime: a panic in a glue hook run by the hub (`switched`, `idle`); aborting\n",
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
            }
            HubStep::Idle(d) => {
                let g = glue();
                hub_hook(|| g.idle(d));
            }
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
    with(|s| s.die());
}

/// Creating a context's stack failed: natively `lthread` throws
/// `lean::exception("failed to create thread: <strerror>")`, which nothing
/// catches: libc++ reports it and aborts (nothing is flushed). glibc's
/// `pthread_create` fails with `EAGAIN` when it cannot map a stack.
pub(crate) fn thread_create_failed() -> ! {
    use std::io::Write;
    let _ = std::io::stderr().write_all(
        b"libc++abi: terminating due to uncaught exception of type lean::exception: failed to create thread: Resource temporarily unavailable\n",
    );
    std::process::abort()
}

/// How long a context able to run, or a task a worker has picked, waits
/// before an output of the running context lets it go first: natively it
/// runs meanwhile on its own thread, and would by then have got past
/// anything that takes no time (thread wake-ups take microseconds).
pub(crate) const STALE: Duration = Duration::from_millis(5);
