//! Threads mode: Lean's task manager on real threads (feature `threads`;
//! docs/threads.md). A port of Lean 4.34.0's `task_manager`
//! (`src/runtime/object.cpp` 758-1098):
//! - a pool of standard workers, made on demand up to `LEAN_NUM_THREADS`
//!   (else the number of online processors), each with Lean's thread stack
//!   size (`lthread`: 1 GiB, or `LEAN_STACK_SIZE_KB` plus 128 KiB);
//! - a thread of its own for each dedicated task (priority above
//!   `Task.Priority.max`);
//! - one more worker while a pool task waits in `Task.get` or `IO.wait`
//!   (`wait`); `IO.waitAny` keeps its worker (`wait_any`), as natively;
//! - one lock over the task table (`task.rs` has the lock order and who
//!   wakes whom).
//!
//! A task that blocks blocks its own thread: there are no coroutines, so no
//! `Glue::suspend`, no yield points and none of the single-thread
//! scheduler's emulation rules (the pure-task rule, the polling counts, the
//! lone worker's latency, the effect points' 5 ms rule).
//!
//! `sched` re-exports this module under the single-thread scheduler's names
//! (`src/sched/threads.rs`), so a translator's glue calls `sched::spawn`,
//! `sched::wait`, `sched::release`, ... in both modes. What differs:
//! - `Job` is `Send` (`Box<dyn FnOnce() -> Outcome + Send>`), the one place
//!   where the bound appears (leanrs's point 2): a worker runs and drops it;
//! - the glue is `Arc<dyn Glue>`, `Glue: Send + Sync`, with the hooks
//!   `thread_start`, `thread_end`, `task_begin` and `task_end`;
//! - the glue's own objects that a thread may wait for (a thunk forced on
//!   two threads, a lazy constant) block the OS thread, with std's
//!   `OnceLock` or a lock, as native's `lean_obj_once`; no crate API
//!   (`block_sync`, `wake`, `CtxId` are the single-thread scheduler's);
//! - `IO.Ref`: the 4.35 rule as a lock and a condition variable, `Ref`.
//!
//! Native bugs are not copied (docs/lean-bugs.md): a pool task enqueued
//! after `main` returned runs (LB-13, `finish`); `Promise.result!` on a
//! dropped promise wakes the waiters of the walks it blocks (LB-32,
//! `option_get_or_block`); `Ref` never loses a `set` (LB-01) and its `swap`
//! never returns its own argument (LB-18).

mod refs;
pub mod sync;
mod task;
#[cfg(test)]
mod tests;

pub use super::common::{TaskState, GET_IN_SYNC_TASK, PROMISE_BEFORE_MANAGER, PROMISE_DROPPED};
pub use refs::Ref;

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

/// A task's computation. It fills the translator's result slot, then
/// returns `Outcome::Done`; or, for a bind task whose function returned a
/// task that has not finished, `Outcome::Continue`. `Send`: a worker thread
/// runs it, and the thread that releases the task may drop it.
pub type Job = Box<dyn FnOnce() -> Outcome + Send>;

/// What a job did.
pub enum Outcome {
    /// It has stored the task's value: the task has finished.
    Done,
    /// A bind task's function returned `TaskId`, which has not finished: the
    /// task waits for it, keeping its priority and `sync` flag, and then
    /// runs `Job` (which copies that task's value), as Lean's
    /// `task_bind_fn1` sets the task's closure again.
    Continue(TaskId, Job),
}

/// A task: a serial number, never reused within a run, valid on every
/// thread. The id of a task that has finished, or was released, answers as
/// finished, so the glue may keep passing it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskId(u64);

impl TaskId {
    /// `Task.pure` (`lean_task_pure`) and every other finished task: the id
    /// the glue passes for a task whose slot holds its value (docs/sched.md,
    /// "The glue", item 3). It never enters the scheduler.
    pub const FINISHED: TaskId = TaskId(0);

    /// The id as a word (for a translator that stores it in its own values).
    pub fn to_bits(self) -> u64 {
        self.0
    }
    pub fn from_bits(b: u64) -> TaskId {
        TaskId(b)
    }
}

/// The translator's part of threads mode: hooks for what a thread owns
/// natively. Every method is called with no lock of the scheduler held, so
/// it may call the scheduler's functions; a Rust panic in one aborts the
/// process.
pub trait Glue: Send + Sync {
    /// A thread the task manager made (a standard worker, a dedicated
    /// task's thread) starts, before it runs a task. With the feature
    /// `stack-overflow`, the crate has already installed Lean's report on
    /// it (`install_stack_overflow_handler`).
    fn thread_start(&self) {}

    /// Such a thread ends (at `finish`, or after its dedicated task).
    fn thread_end(&self) {}

    /// A task starts running on the calling thread. `own_thread`: natively
    /// on a thread of its own (a worker's pool task, a dedicated task), so
    /// with the process's standard streams: the glue gives it fresh slots
    /// (`io::streams::swap_context` with `StreamContext::default()`),
    /// which is one of native's schedules (a fresh worker's), as in the
    /// single-thread scheduler (docs/threads.md, 1.4). Otherwise a `sync`
    /// task on the current thread (a `sync := true` dependent, a task at
    /// priority `LEAN_SYNC_PRIO`), sharing its streams.
    fn task_begin(&self, _own_thread: bool) {}

    /// The task started by the matching `task_begin` has finished (its
    /// `sync` dependents have run), or waits for the task its bind function
    /// returned.
    fn task_end(&self, _own_thread: bool) {}
}

/// `lean_init_task_manager` (called by Lean's generated `main` after the
/// module initializers): from now on tasks run on the task manager's
/// threads. `LEAN_NUM_THREADS=0` means no task manager: tasks keep running
/// at once, and `IO.Promise.new` is Lean's internal panic. `start_with(glue,
/// lean_num_threads(), thread_stack_size())`. Call it on the thread that
/// runs `main` (its `thread_number` is 0), and not from inside a job.
pub fn start(glue: Arc<dyn Glue>) {
    start_with(
        glue,
        crate::sched::lean_num_threads(),
        crate::sched::thread_stack_size(),
    );
}

/// `start` with the number of the task manager's standard workers and the
/// stack size of each thread it makes given, for a translator with rules of
/// its own (`std::thread::Builder::stack_size`). Calling it again replaces
/// the glue and both numbers (the stack size applies to threads made
/// afterwards).
pub fn start_with(glue: Arc<dyn Glue>, workers: u32, stack_size: usize) {
    let sh = task::bind_global();
    task::configure(&sh, glue, workers, stack_size);
    // Lean's stack-overflow report covers this thread too, if the glue
    // installed it.
    #[cfg(feature = "stack-overflow")]
    super::stack_overflow::on_scheduler_thread();
}

/// The final run (`lean_finalize_task_manager`, after `main` returns,
/// whatever it returned; `~task_manager`, `object.cpp` 972-988): Lean's
/// shutdown flag is set (`IO.checkCanceled` is true in tasks from now on),
/// the workers finish the queued tasks, and the call returns once no task is
/// queued, no standard worker is left and every dedicated task has run to
/// completion; the threads are joined. A pool task enqueued meanwhile, also
/// once no worker is left, gets a worker and runs, which native Lean never
/// does (LB-13 in docs/lean-bugs.md). A task whose dependency never finishes
/// (an unresolved promise) is not waited for; a task blocked for good keeps
/// the process alive, as natively. Afterwards there is no task manager
/// (tasks run at once).
///
/// With the feature `io`, it then waits for the io layer's dedicated tasks
/// (`io::exit::after_main`: `IO.Process.output`'s standard-output readers),
/// as in the single-thread scheduler. Then the glue flushes the streams and
/// exits (`io::exit::exit`): the exit order of decisions Q5 refinement A
/// (leanrs's D34 (c)). `IO.Process.exit` and an internal panic do not call
/// it, and wait for no task (LB-29).
pub fn finish() {
    task::with_shared(|sh| {
        if let Some(sh) = sh {
            task::finish(sh);
        }
    });
    #[cfg(feature = "io")]
    crate::io::exit::after_main();
}

/// `lean_task_spawn_core(c, prio, keep_alive)`: `Task.spawn` (`keep_alive`
/// false) and `IO.asTask` (true). Without a task manager (during module
/// initialization, `LEAN_NUM_THREADS=0`, after `finish`) the job runs at
/// once and the task is finished; at priority `LEAN_SYNC_PRIO` it runs at
/// once as a task on the calling thread; above `Task.Priority.max` on a
/// thread of its own; otherwise it is queued for the pool.
pub fn spawn(job: Job, prio: u64, keep_alive: bool) -> TaskId {
    let r = task::with_shared(|sh| match sh {
        Some(sh) => task::spawn(sh, job, prio, keep_alive),
        None => Err(job),
    });
    match r {
        Ok(id) => id,
        Err(job) => {
            let _ = job();
            TaskId::FINISHED
        }
    }
}

/// Whether a dependent of `src` is not a task at all: without a task
/// manager, or with `sync := true` on a finished task, Lean applies the
/// function at once in the calling thread (`lean_task_map_core`,
/// `lean_task_bind_core`). The translator then does that itself instead of
/// calling `depend`. (Another thread may finish `src` right after a false
/// answer: `depend` then runs a `sync` dependent at once, as Lean's
/// `add_dep` does.)
pub fn dependent_runs_now(src: TaskId, sync: bool) -> bool {
    task::with_shared(|sh| match sh {
        Some(sh) => task::dependent_runs_now(sh, src, sync),
        None => true,
    })
}

/// `Task.map`/`Task.bind` (`keep_alive` false), `IO.mapTask`/`IO.bindTask`
/// (true): a new task running `job` once `src` has finished (it reads
/// `src`'s value), as Lean's `add_dep`; when `src` finishes, a `sync`
/// dependent runs there and then on the finishing thread, the others are
/// queued. If `src` has finished already, it is queued now (a `sync` one
/// runs now, here). Without a task manager, the job runs at once.
pub fn depend(src: TaskId, job: Job, prio: u64, sync: bool, keep_alive: bool) -> TaskId {
    let r = task::with_shared(|sh| match sh {
        Some(sh) => task::depend(sh, src, job, prio, sync, keep_alive),
        None => Err(job),
    });
    match r {
        Ok(id) => id,
        Err(job) => {
            let _ = job();
            TaskId::FINISHED
        }
    }
}

/// `Task.get`/`IO.wait` (`lean_task_get`, `task_manager::wait_for`): the
/// calling thread blocks until task `id` has finished. Inside a pool task,
/// the pool's limit rises by one meanwhile, and a worker is made if none is
/// idle, so the pool cannot starve. A task needed by its own computation
/// waits forever, as natively. The glue reports `GET_IN_SYNC_TASK` first
/// when `in_sync_task()` (docs/sched.md, "The glue", item 3).
pub fn wait(id: TaskId) {
    task::with_shared(|sh| {
        if let Some(sh) = sh {
            task::wait(sh, id);
        }
    })
}

/// Whether task `id` has finished.
pub fn is_finished(id: TaskId) -> bool {
    task::with_shared(|sh| sh.is_none_or(|sh| task::is_finished(sh, id)))
}

/// `IO.getTaskState` (`lean_io_get_task_state_core`, `get_task_state`):
/// native's answer, with no polling rule: queued or waiting for its source
/// is `waiting`; running, or an unresolved promise, is `running`.
pub fn state(id: TaskId) -> TaskState {
    task::with_shared(|sh| match sh {
        Some(sh) => task::state(sh, id),
        None => TaskState::Finished,
    })
}

/// `IO.waitAny` (`lean_io_wait_any_core`, `task_manager::wait_any`): the
/// index of the first finished task of `ids`, in list order; else the
/// calling thread blocks until a task's finish notifies, and looks again.
/// A pool task keeps its worker meanwhile, as natively.
pub fn wait_any(ids: &[TaskId]) -> usize {
    assert!(!ids.is_empty(), "lean-runtime: IO.waitAny of an empty list");
    task::with_shared(|sh| match sh {
        Some(sh) => task::wait_any(sh, ids),
        // without a task manager every task has finished
        None => 0,
    })
}

/// `IO.cancel` (`lean_io_cancel_core`): set the cancellation flag of an
/// unfinished task. When a canceled task finishes, its dependents created
/// while it was unfinished are canceled too (`handle_finished`).
pub fn cancel(id: TaskId) {
    task::with_shared(|sh| {
        if let Some(sh) = sh {
            task::cancel(sh, id);
        }
    })
}

/// `IO.checkCanceled` (`lean_io_check_canceled_core`): inside a task,
/// whether it was canceled or the program is shutting down (`finish`);
/// false outside tasks. Lock-free.
pub fn check_canceled() -> bool {
    task::check_canceled()
}

/// Lean's `deactivate_task`: the translator's last reference to task `id`
/// is gone, on any thread (call it for every task, IO tasks included). A
/// pure task that has not started is deleted and never runs; a running pure
/// task runs to its end, canceled; an IO task runs to completion. The
/// finish of a task released before it finished notifies nobody (natively
/// it is deleted then, `m_deleted`, without `resolve_core`'s `notify_all`).
/// A deleted task's job is dropped here, outside the scheduler's lock, so it
/// may release its own source in turn. Nothing for a finished task's id.
pub fn release(id: TaskId) {
    if id == TaskId::FINISHED {
        return;
    }
    let job = task::with_shared(|sh| sh.and_then(|sh| task::release(sh, id)));
    drop(job);
}

/// Whether the innermost task running on this thread is a `sync` one: a
/// `sync := true` dependent, or a task at priority `LEAN_SYNC_PRIO`. Native
/// `Task.get` (and `IO.wait`) of an unfinished task from such a task prints
/// `GET_IN_SYNC_TASK` as a Lean panic before it waits
/// (`task_manager::wait_for`); the glue reproduces it.
pub fn in_sync_task() -> bool {
    task::in_sync_task()
}

/// `IO.getTID` inside tasks, as the single-thread scheduler offers it: the
/// number to add to `main`'s thread id, 0 on the thread that called
/// `start`, and a number of its own on every other thread. Native's answer
/// is the thread's `gettid` (`lean_io_get_tid`), which a glue in threads
/// mode may give instead.
pub fn thread_number() -> u64 {
    task::thread_number()
}

/// Whether the task manager runs (`g_task_manager`).
pub fn manager_running() -> bool {
    task::with_shared(|sh| sh.is_some_and(|sh| sh.started()))
}

/// `IO.Promise.new` (`lean_promise_new`), and the task that
/// `IO.Promise.result?` returns: a new unresolved promise's task (see the
/// single-thread scheduler's `promise_new`). Before the task manager runs,
/// Lean's internal panic (`PROMISE_BEFORE_MANAGER`), which the glue
/// reports.
pub fn promise_new() -> Result<TaskId, &'static str> {
    task::with_shared(|sh| sh.and_then(task::promise_new)).ok_or(PROMISE_BEFORE_MANAGER)
}

/// `IO.Promise.resolve` (`task_manager::resolve`), and the resolution with
/// `none` when the last reference to an unresolved promise goes
/// (`deactivate_promise`), on any thread: if the promise is unresolved,
/// `store` stores its value in the translator's slot (on the calling thread,
/// outside the scheduler's lock), then its dependents are walked on the
/// calling thread (its `sync` dependents run here), and its waiters wake.
/// Only the first resolution counts: false (and `store` not called) if it
/// was resolved already, or once another thread's resolution has stored.
pub fn resolve(id: TaskId, store: impl FnOnce()) -> bool {
    if id == TaskId::FINISHED {
        return false;
    }
    let mut store = Some(store);
    task::with_shared(|sh| match sh {
        Some(sh) => task::resolve(sh, id, || (store.take().expect("called once"))()),
        None => false,
    })
}

/// `IO.Option.getOrBlock!` (`lean_option_get_or_block`, `io.cpp`), the
/// function that `Promise.result!` maps over `Promise.result?` with `sync :=
/// true`: the value of `some`. On `none` (the promise was dropped without
/// ever being resolved), `report` reports the Lean panic `PROMISE_DROPPED`
/// as in the single-thread scheduler; then every waiter looks again, so the
/// waiters of the tasks whose walks are in progress on this thread, among
/// them `result?`'s, wake and see `none` (LB-32 in docs/lean-bugs.md:
/// natively they wake only when another task finishes, or never); then the
/// calling thread blocks forever (`hang`), as natively it sleeps forever,
/// while the others go on.
pub fn option_get_or_block<T>(opt: Option<T>, report: impl FnOnce(&'static str)) -> T {
    match opt {
        Some(v) => v,
        None => {
            report(PROMISE_DROPPED);
            task::with_shared(|sh| {
                if let Some(sh) = sh {
                    task::wake_waiters(sh);
                }
            });
            hang()
        }
    }
}

/// Block the calling thread forever, while the others go on: a thunk forced
/// from its own computation (LB-08: natively it spins), `Promise.result!`
/// on a dropped promise.
pub fn hang() -> ! {
    hang_thread()
}

/// Block the calling thread forever.
pub fn hang_thread() -> ! {
    loop {
        std::thread::park();
    }
}

// ---------------------------------------------------------------------------
// The single-thread scheduler's points, as threads mode has them

/// An effect point of the single-thread scheduler: nothing to do, other
/// threads run in parallel.
#[inline]
pub fn effect() {}

/// A polling point of the single-thread scheduler: nothing to do.
#[inline]
pub fn poll() {}

/// An `ST.Ref` read's polling point of the single-thread scheduler: nothing
/// to do.
#[inline]
pub fn ref_read() {}

/// Whether `ST.Ref` reads are polling points: no effect in threads mode.
#[inline]
pub fn set_ref_read_yields(_on: bool) {}

/// How many `ST.Ref` reads make one polling point in the single-thread
/// scheduler (unused here).
pub const REF_READS_PER_POLL: u32 = 1000;

/// The end of a task's job, right before the glue stores the task's value
/// (docs/sched.md, "The glue", item 3): in the single-thread scheduler the
/// writer threads of the streams the context handed off end first. Threads
/// mode hands off no stream (a dropped stream's close blocks its own thread,
/// the io layer's plain path), so there is nothing to wait for.
#[inline]
pub fn before_task_value() {}

/// A write the glue makes that another thread can see (an `ST.Ref` write, a
/// thunk's store): as `before_task_value`, nothing to wait for.
#[inline]
pub fn before_publish() {}

/// `IO.sleep ms` and `dbgSleep`: the calling thread sleeps
/// (`std::this_thread::sleep_for`).
pub fn sleep_ms(ms: u32) {
    std::thread::sleep(Duration::from_millis(u64::from(ms)));
}

// ---------------------------------------------------------------------------
// The no-suspend scope: callable as in the single-thread scheduler

thread_local! {
    /// The depth of the no-suspend scopes this thread is in.
    static NO_SUSPEND: Cell<u32> = const { Cell::new(0) };
}

/// Enter a no-suspend scope (nestable), as in the single-thread scheduler,
/// where it keeps the io layer's calls from suspending the running context
/// inside a translator's free or drop path. In threads mode nothing ever
/// suspends and every io call takes the plain path, so the scope changes no
/// behaviour; the depth is kept per thread, so `in_no_suspend` answers
/// truly. A drop walk on a worker thread, and the promises it resolves
/// after the walk (`resolve`, which may run on any thread), need nothing
/// more.
#[inline]
pub fn enter_no_suspend() {
    let _ = NO_SUSPEND.try_with(|n| n.set(n.get() + 1));
}

/// Leave the innermost no-suspend scope: a decrement, safe in any `Drop`.
#[inline]
pub fn leave_no_suspend() {
    let _ = NO_SUSPEND.try_with(|n| n.set(n.get().saturating_sub(1)));
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

/// Whether a blocking call must let other contexts run: never in threads
/// mode (a blocking call blocks its own thread, as natively).
#[inline]
pub fn io_cooperative() -> bool {
    false
}

/// Whether the single-thread scheduler's cooperative paths may be needed:
/// never in threads mode.
#[inline]
pub fn coop_possible() -> bool {
    false
}

/// The stack of a context, for a stack-overflow report, as the
/// single-thread scheduler describes it: its guard page(s) `[guard_lo,
/// guard_hi)` and its top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StackBounds {
    pub guard_lo: usize,
    pub guard_hi: usize,
    pub top: usize,
}

/// The stack of the context running on the calling thread: always `None`
/// in threads mode, where every task runs on a thread's own stack.
pub fn running_stack() -> Option<StackBounds> {
    None
}
