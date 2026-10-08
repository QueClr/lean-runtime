//! The task scheduler: Lean's task manager on one thread.
//!
//! Tasks are deferred until needed and run as coroutines that can block and
//! resume (`ctx`, through corosensei); yield points at effect and polling
//! operations let what natively runs in parallel go first; promises;
//! `Std.Sync`'s primitives (`sync`); and Lean's behaviour at exit (pending IO
//! tasks run, dropped queued pure tasks never do, and the streams are flushed
//! only after the tasks have been joined). The model is lean2rr's (its
//! `leanrt`: `sched.rs`, `task.rs`, `sync.rs`), made independent of either
//! translator's representation of values; docs/sched.md describes it, the
//! glue a translator writes, and where it differs from lean2rr's.
//!
//! All of it runs on one thread: the one that runs `main` (`start` is called
//! there). Every function may switch to another context before it returns,
//! except where its documentation says otherwise.
//!
//! A translator's glue:
//! - implements `Glue` (its `suspend` is the one `unsafe` step) and calls
//!   `start` after the module initializers, `finish` after `main`, and then
//!   flushes its streams and exits;
//! - keeps each task's value in its own object, filled by the task's `Job`,
//!   and calls `release` when its last reference to a task goes;
//! - calls the yield points (`effect`, `poll`, `ref_read`, `sleep_ms`) from
//!   its externs;
//! - waits for its own objects through the wait cores (docs/sched.md, "The
//!   wait cores"): `Gate` or the keyed claims for a thunk, a static or a
//!   constant another context computes (`wait.rs`), `Ref` or `ref_keyed`
//!   for `ST.Ref` (`refs.rs`), and `defer` and `run_deferred` (or
//!   `DrainScope`) for a promise dropped in its free (`drain.rs`);
//! - calls the drain-end hook `after_drain` at the end of each of its
//!   drains, after `run_deferred` (`DrainScope` does both).

mod common;
mod ctx;
// The deferred promise resolutions of a translator's drains (wait-1, core
// 3.3; both modes).
mod drain;
mod env;
mod reactor;
// The single-thread `ST.Ref` under Lean 4.35's rule, as an object and as
// keyed functions (wait-1, core 3.2).
mod refs;
#[cfg(feature = "stack-overflow")]
mod stack_overflow;
pub mod sync;
mod task;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wait_tests;
// Each context's and each emulated worker's standard streams and `errno`
// (review AR-24).
#[cfg(feature = "io")]
mod slots;
pub mod uv;
// The process-wide part of `uv`'s signal delivery, shared with threads mode.
mod uv_signals;
// Waiting for a computation another context runs (wait-1, core 3.1).
mod wait;

pub use common::{await_task, thread_create_failed};
pub use ctx::{
    running_stack, switch_is_event_loop, CtxId, Glue, StackBounds, Suspend, Yielder, MAIN,
};
pub use drain::{
    defer, deferred_pending, run_deferred, Deferred, DrainScope, RESOLVE_IN_NO_SUSPEND,
};
pub use env::{hardware_concurrency, lean_num_threads, thread_stack_size};
#[cfg(feature = "io")]
pub(crate) use reactor::block_until;
/// `net`'s externs take the loop's lock natively, as `sched::uv`'s do.
#[cfg(feature = "net")]
pub(crate) use reactor::catch_up;
pub(crate) use reactor::in_no_suspend_scope;
pub use reactor::{
    coop_possible, enter_no_suspend, in_no_suspend, io_cooperative, leave_no_suspend, no_suspend,
    poll_fds, timer_start, timer_stop, unwatch, wait_fd, watch, watch_modify, Interest,
    NoSuspendGuard, PollItem, Ready, TimerId, WatchId,
};
pub use refs::{ref_keyed, Ref};
#[cfg(feature = "stack-overflow")]
pub use stack_overflow::install_stack_overflow_handler;
pub use wait::{
    done_keyed, step_keyed, wait_running_keyed, Gate, Step, WaitList, WAIT_IN_NO_SUSPEND,
};

/// Turn on the cooperative paths as the first task does (the io layer's
/// unit tests).
#[cfg(all(test, feature = "io"))]
pub(crate) fn reactor_coop_on_for_tests() {
    reactor::coop_on();
}
pub use task::{
    cancel, check_canceled, current_context, depend, dependent_runs_now, effect, end_running_task,
    finish, full_slot_finished, in_sync_task, is_finished, manager_running, option_get_or_block,
    poll, promise_new, release, resolve, running_worker, sleep_ms, spawn, state, thread_number,
    tid_offset, wait, wait_any, Job, Outcome, TaskId, TaskState, GET_IN_SYNC_TASK,
    PROMISE_BEFORE_MANAGER, PROMISE_DROPPED,
};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct Sched {
    pub(crate) cx: ctx::Contexts,
    pub(crate) tk: task::Tasks,
    /// The event loop (sched-io).
    pub(crate) ev: reactor::Reactor,
}

thread_local! {
    static SCHED: RefCell<Sched> = RefCell::new(Sched {
        cx: ctx::Contexts::new(),
        tk: task::Tasks::new(),
        ev: reactor::Reactor::default(),
    });
}

/// Run `f` on the scheduler's state. `f` never runs translated code, a
/// glue hook or a destructor of the translator's values, and never switches
/// contexts, so no borrow is held across them.
pub(crate) fn with<R>(f: impl FnOnce(&mut Sched) -> R) -> R {
    SCHED.with(|s| f(&mut s.borrow_mut()))
}

/// A point where the running context publishes something or may suspend:
/// first, the writer threads of the streams its drops handed off
/// (`io::coop::hand_off`) end, letting the other contexts run meanwhile, as
/// natively its thread was inside those streams' `fclose` until then and
/// could do nothing else (AR-8; review RFX1-07; leanrs's re-check of
/// a771e57). Called by the scheduler's own points (effect points, polls,
/// sleeps, waits, `hang`, promise resolutions, task creations, `Std.Sync`
/// operations, the end of a task's job and of `main`) and, through the
/// public [`before_publish`] and [`before_task_value`], by the glue. One
/// relaxed load when no writer exists; nothing without the feature `io`.
/// It waits for nothing in a no-suspend scope, while the context holds a
/// stream lock, or while a panic unwinds (the next point waits instead).
#[inline]
pub(crate) fn writers_point() {
    #[cfg(feature = "io")]
    crate::io::coop::join_own_writers(crate::io::coop::JoinAt::End);
}

/// The end of a task's job, right before the glue stores the task's value
/// (item 3 of "The glue" in `docs/sched.md`): a [`writers_point`], so a
/// context that waits for the task sees the bytes of the streams the task
/// handed off delivered, as natively its `fclose` had returned. A glue that
/// does not call it lets a waiter see the value before `run_task`'s own
/// wait, with those bytes still on their way.
#[inline]
pub fn before_task_value() {
    writers_point();
}

/// A write the glue makes that another context can see (an `ST.Ref`'s
/// `set`, `swap`, `take`, `modify`'s store, in a program with tasks): a
/// [`writers_point`] (item 7 of "The glue").
#[inline]
pub fn before_publish() {
    writers_point();
}

/// The drain-end hook (item 11 of "The glue" in `docs/sched.md`, review
/// HR-01..03): the end of a drain of the running context (a translator's
/// free walk), once its no-suspend scope has been left and its deferred
/// resolutions have run (`run_deferred`, each of which waited only for the
/// writers handed off before it, review RF14-07). The writer threads of the
/// streams the drain's drops handed off (`io::coop::hand_off`) end here,
/// while the other contexts run, as natively the drop's `fclose` had
/// returned before the free went on: so glue code that reads state after
/// the drain (a promise's resolution, a task's state for
/// `dependent_runs_now`, a lock) reads it as natively, not before a
/// writers point that lets other contexts change it. Call it at every
/// drain's end, where a switch is allowed (as `run_deferred`): whenever
/// the running context has a writer that runs, it lets the other contexts
/// run until that writer ends. One relaxed load while no writer runs in
/// the process: the count of running writers is process-wide, so while
/// another context's (or another thread's) writer runs, the call takes the
/// slow path (a lock and a scan of the writers) and waits for nothing
/// (review RF14-06). It waits for nothing inside a no-suspend scope (an
/// outer drain's end waits instead), while the context holds a stream
/// lock, or while a panic unwinds: the next writers point waits then.
/// `DrainScope`'s outermost drop calls it. In threads mode it does nothing
/// (no stream is handed off).
#[inline]
pub fn after_drain() {
    writers_point();
}

/// The running context, where the scheduler's state can be read: `None`
/// during the destruction of the thread's locals, or inside [`with`] (a
/// destructor run there).
#[cfg(feature = "io")]
pub(crate) fn running_context() -> Option<CtxId> {
    SCHED
        .try_with(|s| s.try_borrow().ok().map(|s| s.cx.cur))
        .ok()
        .flatten()
}

/// Whether the scheduler's state is still there: not during the destruction
/// of the thread's locals at exit, when a translator's global holding a task
/// or a promise is destructed (natively nothing is destructed at exit).
pub(crate) fn alive() -> bool {
    SCHED.try_with(|_| ()).is_ok()
}

pub(crate) fn glue_opt() -> Option<Rc<dyn Glue>> {
    with(|s| s.cx.glue.clone())
}

pub(crate) fn glue() -> Rc<dyn Glue> {
    glue_opt().expect("lean-runtime: sched::start was not called")
}

/// `lean_init_task_manager` (called by Lean's generated `main` after the
/// module initializers and `lean_io_mark_end_initialization`): from now on
/// tasks are deferred ([`start_lazy`] builds the scheduler only when the
/// program first needs it). `LEAN_NUM_THREADS=0` means no task manager: tasks keep
/// running at once, and `IO.Promise.new` is Lean's internal panic. Reads
/// `LEAN_NUM_THREADS` and `LEAN_STACK_SIZE_KB` (`lean_run_main` reads the
/// latter at this point): `start_with(glue, lean_num_threads(),
/// thread_stack_size())`. Call it on the thread that runs `main`.
pub fn start(glue: Rc<dyn Glue>) {
    start_with(glue, lean_num_threads(), thread_stack_size());
}

/// `start` with the number of the task manager's workers and the stack size
/// of each context given, for a translator with rules of its own. Lean's are
/// `lean_num_threads()` (`LEAN_NUM_THREADS`, else the online processors) and
/// `thread_stack_size()`: the size of Lean's threads (`lthread`), 1 GiB on
/// 64-bit targets, or `LEAN_STACK_SIZE_KB` rounded down to 4 KiB plus
/// 128 KiB. The size is rounded up to 64 KiB, and a guard page lies below
/// it.
///
/// Calling it again replaces the glue and both numbers. A new stack size
/// applies to contexts started afterwards: the stacks kept for reuse are
/// dropped, and a context still running on a stack of the old size does not
/// return it to the pool when it ends.
///
/// Two stacks do not follow `stack_size`. The event loop's context gets at
/// least 1 GiB, as natively libuv's loop thread has 1 GiB whatever
/// `LEAN_STACK_SIZE_KB` says. And a needed task runs on its waiter's stack
/// only if that stack has a native worker's room left: `thread_stack_size()`
/// read here, at most `stack_size`, less its slack (1 MiB, at most a
/// sixteenth of it; docs/sched.md, "The stack room of a run on the waiter's
/// stack").
pub fn start_with(glue: Rc<dyn Glue>, workers: u32, stack_size: usize) {
    LAZY.with(|l| l.borrow_mut().take());
    STATE.with(|st| st.set(STARTED | if workers > 0 { DEFERS } else { 0 }));
    with(|s| {
        s.cx.glue = Some(glue);
        s.cx.pool_limit = workers;
        // a native worker's stack: the room a task needs to run on its
        // waiter's stack (`room_to_run_here`, hunt HSK-01)
        s.cx.set_stack_size(stack_size, thread_stack_size());
        s.tk.started = workers > 0;
    });
    task::MANAGER.with(|m| m.set(workers > 0));
    // Lean's stack-overflow report covers this thread too, if the glue
    // installed it (`install_stack_overflow_handler`).
    #[cfg(feature = "stack-overflow")]
    stack_overflow::on_scheduler_thread();
}

// ---------------------------------------------------------------------------
// The lazy start (lean2rr's `task::start` and `ensure_started`; audit item
// 4.5)

/// [`STATE`]: `start` or `start_with` has run on this thread (directly or
/// through [`ensure_started`]).
const STARTED: u8 = 1;
/// [`STATE`]: [`start_lazy`] has run and the scheduler is not built yet.
const PENDING: u8 = 2;
/// [`STATE`]: the task manager runs or will run at the lazy start: new tasks
/// are deferred ([`deferring`]).
const DEFERS: u8 = 4;

thread_local! {
    /// The lazy start's state on this thread ([`STARTED`], [`PENDING`],
    /// [`DEFERS`]): one byte, so that [`ensure_started`], [`sched_started`]
    /// and [`deferring`] inline down to its load.
    static STATE: Cell<u8> = const { Cell::new(0) };
    /// What [`start_lazy`] was given, until [`ensure_started`] starts the
    /// scheduler with it.
    static LAZY: RefCell<Option<LazyStart>> = const { RefCell::new(None) };
}

/// [`start_lazy`]'s glue, workers and stack size.
type LazyStart = (Rc<dyn Glue>, u32, usize);

/// `lean_init_task_manager` at `main`'s start, with the scheduler itself
/// built only when the program first needs it (lean2rr's lazy start): the
/// number of workers and the contexts' stack size are taken now (Lean reads
/// `LEAN_NUM_THREADS` and `LEAN_STACK_SIZE_KB` when `main` starts:
/// [`lean_num_threads`], [`thread_stack_size`]), and [`ensure_started`]
/// calls `start_with(glue, workers, stack_size)` at the first task, promise,
/// `Std.Sync` object or operation, timer, signal watcher or socket. The
/// crate's own entry points for those call it ([`spawn`], [`depend`],
/// [`dependent_runs_now`], [`promise_new`], every method of [`sync`]'s
/// objects, `uv::loop_configure`, `uv::loop_alive`, `uv::Timer::new`,
/// `uv::Signal::new`, and `net`'s `TcpSocket::new`, `UdpSocket::new`,
/// `dns::get_addr_info` and `dns::get_name_info`); a glue calls it before
/// anything of its own that needs the scheduler.
///
/// So a program that makes none of them builds no scheduler state, context
/// or event loop, and pages in none of their code. Until the start nothing
/// could have been deferred or run elsewhere, so the two are the same:
/// - [`deferring`] is true from now on when `workers > 0` (natively the
///   task manager runs from `main`'s start), while [`manager_running`]
///   stays false until the scheduler is built (it means "built with
///   workers": the crate reads it to know whether contexts exist);
/// - [`finish`] after `main` waits for the io layer's dedicated tasks and
///   the streams handed off, as without a scheduler, and builds nothing;
/// - at the start, `ST.Ref` reads become polling points
///   ([`set_ref_read_yields`]`(true)`): before it no other context exists,
///   so no read needs to poll (decisions Q5 refinement B, decided when the
///   program first needs a scheduler);
/// - `Std.Sync`'s operations start it too, since a wait needs the
///   scheduler's contexts; a lock's owner does not depend on the start (it
///   names the OS thread, `sync`'s module comment, AR-39), so an object an
///   initializer made, locked by `main` before its first task and again
///   after it, has one owner (lean2rr's review RS4-05).
///
/// Call it on the thread that runs `main` (inside `io::startup::run_main`'s
/// body, with `main` and `finish`: the state is this thread's), after the
/// module initializers (during them nothing is started: Lean has no task
/// manager then). Once the scheduler has started on this thread, it is
/// `start_with` (which replaces the glue and both numbers). Single-thread
/// scheduler only: threads mode (`sched::mt`) has no lazy start, and starts
/// at once.
pub fn start_lazy(glue: Rc<dyn Glue>, workers: u32, stack_size: usize) {
    if sched_started() {
        start_with(glue, workers, stack_size);
        return;
    }
    LAZY.with(|l| *l.borrow_mut() = Some((glue, workers, stack_size)));
    STATE.with(|st| st.set(PENDING | if workers > 0 { DEFERS } else { 0 }));
}

/// Start the scheduler now if [`start_lazy`] is waiting for it on this
/// thread (then `ST.Ref` reads become polling points); otherwise nothing:
/// before `start_lazy` (the module initializers), after the start, or for a
/// glue that calls `start` itself. One thread-local load on the fast path.
#[inline]
pub fn ensure_started() {
    if STATE.with(Cell::get) & PENDING != 0 {
        start_pending();
    }
}

#[cold]
#[inline(never)]
fn start_pending() {
    if let Some((glue, workers, stack_size)) = LAZY.with(|l| l.borrow_mut().take()) {
        start_with(glue, workers, stack_size);
        set_ref_read_yields(true);
    }
}

/// Whether the scheduler has started on this thread: `start` or
/// `start_with` ran, directly or through [`ensure_started`] (also with no
/// workers, where tasks run at once). Before that, [`current_context`] would
/// build the scheduler's state for nothing: the running context is
/// [`MAIN`].
#[inline]
pub fn sched_started() -> bool {
    STATE.with(Cell::get) & STARTED != 0
}

/// Whether new tasks are deferred (`Task.spawn` makes a task instead of
/// running the function at once): the task manager runs ([`manager_running`])
/// or will at the lazy start ([`start_lazy`] with workers). lean2rr's
/// generated code asks it before it registers a task.
#[inline]
pub fn deferring() -> bool {
    STATE.with(Cell::get) & DEFERS != 0
}

/// [`start_lazy`] ran and the scheduler was never built (for [`finish`]).
pub(crate) fn start_pending_only() -> bool {
    STATE.with(Cell::get) & PENDING != 0
}

static REF_YIELDS: AtomicBool = AtomicBool::new(false);

/// Whether `ST.Ref` reads are polling points (`ref_read`). The translator
/// turns it on for programs that create tasks, which it knows at
/// translation time (decisions Q5 refinement B), so that a loop polling a
/// reference set by a task ends; programs without tasks pay nothing.
pub fn set_ref_read_yields(on: bool) {
    REF_YIELDS.store(on, Ordering::Relaxed);
}

/// How many `ST.Ref` reads make one polling point (`ref_read`).
pub const REF_READS_PER_POLL: u32 = 1000;

thread_local! {
    /// The reads left before the next polling point (`ref_read`).
    static REF_READS_LEFT: Cell<u32> = const { Cell::new(0) };
}

/// An `ST.Ref` read (`lean_st_ref_get`, and the reads of `modify`, `swap`,
/// `take`), if `set_ref_read_yields` turned them on: every
/// `REF_READS_PER_POLL`-th read on a thread is a polling point (`poll`), the
/// first one included. A loop polling a reference that another context sets
/// still reaches a polling point every so many reads, so it ends; the reads
/// in between cost a thread-local countdown, not `poll`'s clock read and
/// queue scan (review RS1S-03 of sched-1).
#[inline]
pub fn ref_read() {
    if REF_YIELDS.load(Ordering::Relaxed) {
        let due = REF_READS_LEFT.with(|n| match n.get() {
            0 => {
                n.set(REF_READS_PER_POLL - 1);
                true
            }
            k => {
                n.set(k - 1);
                false
            }
        });
        if due {
            ref_read_poll();
        }
    }
}

/// Every `REF_READS_PER_POLL`-th read of [`ref_read`]: out of line, so the
/// reads in between inline down to the flag's load and the countdown
/// (review AR-23). `extern "C"`, so it cannot unwind (review AR-28), as
/// `task::poll_check`.
#[cold]
#[inline(never)]
extern "C" fn ref_read_poll() {
    poll();
}

/// Block the running context until another wakes it (`wake`): the waiting
/// primitive for the glue's own objects (a thunk being forced on another
/// context) and `sync`. Register the context in the object's waiter list
/// (`current_context`) before calling this.
pub fn block_sync() {
    ctx::block(ctx::Wait::Sync);
}

/// Make context `c`, blocked by `block_sync`, able to go on. Does not
/// switch. A context blocked in any other way (a task, a sleep, a hang)
/// is left alone, so a stale entry in a waiter list cannot cut that wait
/// short (review RW1-08); one waiting in another `block_sync` looks again
/// at its own object, as every waiter does.
pub fn wake(c: CtxId) {
    with(|s| s.wake_sync(c));
}

/// Make context `c`, napping in `block_until` (an io wait that looks again,
/// such as a contended `flock`), able to go on at once. Does not switch.
#[cfg(feature = "io")]
pub(crate) fn wake_napping(c: CtxId) {
    with(|s| s.wake_napping(c));
}

/// Whether context `c` is able to run (runnable or running): the io
/// layer's record of a `flock` handoff (`io::flock_last_wake`).
#[cfg(feature = "io")]
pub(crate) fn can_run(c: CtxId) -> bool {
    with(|s| s.can_run(c))
}

/// Whether `main`'s context is able to run (unit tests of the io layer).
#[cfg(all(test, feature = "io"))]
pub(crate) fn main_runnable_for_tests() -> bool {
    with(|s| s.cx.ctxs[MAIN].status == ctx::Status::Runnable)
}

/// Wait forever on the running context, while the others go on: a thunk
/// forced from its own computation, tasks waiting for each other (natively
/// that thread waits forever).
pub fn hang() -> ! {
    writers_point();
    if manager_running() {
        loop {
            ctx::block(ctx::Wait::Forever);
        }
    }
    hang_thread()
}

/// Wait forever: nothing can go on any more (natively a deadlocked process).
pub fn hang_thread() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
