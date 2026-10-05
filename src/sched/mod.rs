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
//!   its externs.

mod common;
mod ctx;
mod env;
mod reactor;
#[cfg(feature = "stack-overflow")]
mod stack_overflow;
pub mod sync;
mod task;
#[cfg(test)]
mod tests;
// Each context's and each emulated worker's standard streams and `errno`
// (review AR-24).
#[cfg(feature = "io")]
mod slots;
pub mod uv;
// The process-wide part of `uv`'s signal delivery, shared with threads mode.
mod uv_signals;

pub use ctx::{running_stack, CtxId, Glue, StackBounds, Suspend, Yielder, MAIN};
pub use env::{hardware_concurrency, lean_num_threads, thread_stack_size};
#[cfg(feature = "io")]
pub(crate) use reactor::block_until;
/// `net`'s externs take the loop's lock natively, as `sched::uv`'s do.
#[cfg(feature = "net")]
pub(crate) use reactor::catch_up;
pub use reactor::{
    coop_possible, enter_no_suspend, in_no_suspend, io_cooperative, leave_no_suspend, no_suspend,
    poll_fds, timer_start, timer_stop, unwatch, wait_fd, watch, watch_modify, Interest,
    NoSuspendGuard, PollItem, Ready, TimerId, WatchId,
};
#[cfg(feature = "stack-overflow")]
pub use stack_overflow::install_stack_overflow_handler;

/// Turn on the cooperative paths as the first task does (the io layer's
/// unit tests).
#[cfg(all(test, feature = "io"))]
pub(crate) fn reactor_coop_on_for_tests() {
    reactor::coop_on();
}
pub use task::{
    cancel, check_canceled, current_context, depend, dependent_runs_now, effect, end_running_task,
    finish, in_sync_task, is_finished, manager_running, option_get_or_block, poll, promise_new,
    release, resolve, sleep_ms, spawn, state, thread_number, wait, wait_any, Job, Outcome, TaskId,
    TaskState, GET_IN_SYNC_TASK, PROMISE_BEFORE_MANAGER, PROMISE_DROPPED,
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
/// tasks are deferred. `LEAN_NUM_THREADS=0` means no task manager: tasks keep
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
pub fn start_with(glue: Rc<dyn Glue>, workers: u32, stack_size: usize) {
    with(|s| {
        s.cx.glue = Some(glue);
        s.cx.pool_limit = workers;
        s.cx.set_stack_size(stack_size);
        s.tk.started = workers > 0;
    });
    // Lean's stack-overflow report covers this thread too, if the glue
    // installed it (`install_stack_overflow_handler`).
    #[cfg(feature = "stack-overflow")]
    stack_overflow::on_scheduler_thread();
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
/// (review AR-23).
#[cold]
#[inline(never)]
fn ref_read_poll() {
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
/// switch.
pub fn wake(c: CtxId) {
    with(|s| s.wake(c));
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
