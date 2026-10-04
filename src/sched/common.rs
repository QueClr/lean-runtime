//! The plain items both schedulers share: the single-thread `sched` (feature
//! `sched`, `src/sched/task.rs`) and threads mode (feature `threads`,
//! `src/sched/mt/`). A build compiles one of them, and this file with it.

/// `IO.TaskState`, its constructors in Lean's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Waiting = 0,
    Running = 1,
    Finished = 2,
}

/// The message of the Lean panic that native `Task.get` prints when it waits
/// for an unfinished task inside a `sync := true` task
/// (`task_manager::wait_for`, `src/runtime/object.cpp`): see `in_sync_task`.
pub const GET_IN_SYNC_TASK: &str = "`Task.get` called from a `(sync := true)` task";

/// The message of Lean's internal panic for `IO.Promise.new` before the task
/// manager runs (`lean_promise_new`); the glue reports it as Lean's
/// `lean_internal_panic` does.
pub const PROMISE_BEFORE_MANAGER: &str = "`IO.Promise.new` called before the task manager is running; this typically happens when called (directly or transitively, e.g. via `IO.CancelToken.new`) from an `initialize` block. Construct lazily on first use instead.";

/// The message of the Lean panic of `IO.Option.getOrBlock!` on `none`
/// (`lean_option_get_or_block`, `io.cpp`), passed to `lean_panic` with
/// `force_stderr`: see `option_get_or_block`.
pub const PROMISE_DROPPED: &str =
    "PANIC: Promise.result!: promise has been dropped without ever being resolved";

/// Priorities: Lean's 0..=8 (`Task.Priority.max`, `LEAN_MAX_PRIO`), and 9
/// for dedicated tasks (a thread of their own natively).
pub(crate) const PRIOS: usize = 10;

/// Lean passes `lean_unbox(prio)` as an `unsigned`: the priority modulo
/// 2^32, where 2^32-1 is `LEAN_SYNC_PRIO` and above 8 is dedicated.
pub(crate) fn priority(prio: u64) -> (u8, bool) {
    let p = prio as u32;
    if p == u32::MAX {
        (0, true)
    } else {
        ((p as u64).min(PRIOS as u64 - 1) as u8, false)
    }
}
