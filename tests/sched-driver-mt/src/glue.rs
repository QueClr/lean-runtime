//! The glue a translator writes around threads mode (`lean_runtime::sched`
//! in a `threads` build, `sched::mt`; docs/threads.md, 2.4): no `unsafe`
//! (nothing suspends), the opt-in to Lean's stack-overflow report on
//! `main`'s thread (the task manager installs it on every thread it makes),
//! the program's entry and exit, and the five hooks of `sched::Glue`. The
//! parts that do not depend on the scheduler (output, Lean's panics,
//! `IO.Process.exit`, native's startup descriptors) are the single-thread
//! driver's `glue_common.rs`.
//!
//! A translator keeps its own per-thread and per-task state in the hooks.
//! This driver has none, so its hooks check the crate's side of their
//! contract while every case runs:
//! - `thread_start` and `thread_end` pair up on each thread the task manager
//!   makes, and once `finish` has returned every such thread has ended
//!   (`Std.Internal.UV`'s loop thread, which never ends, would be the one
//!   exception; no case of these areas uses it);
//! - `task_begin` and `task_end` pair up on the calling thread, nested (a
//!   `sync` task inside the walk of the task below it), each `task_end`
//!   with its `task_begin`'s `own_thread`, and no task is open at
//!   `thread_end` or after `finish`;
//! - a task with a thread of its own (`own_thread`: a worker's pool task, a
//!   dedicated task) begins only on a thread the manager made, and one that
//!   runs no other task: a worker never runs a task inside another, as
//!   natively, and no such task runs on `main`'s thread;
//! - `workers_end` (review AR-34) comes once, from `finish` on `main`'s
//!   thread with no task open there, after every standard worker made so
//!   far has ended (`thread_end`) and before the dedicated tasks are waited
//!   for; `finish` never returns without it while a task manager runs.
//!
//! A broken rule is a Rust panic in a hook, which aborts the process with
//! the crate's message (on `main`, after `finish`, a panic: status 101), so
//! the case fails.

pub use crate::glue_common::*;
use lean_runtime::io::exit;
use lean_runtime::sched::{self, Glue};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

thread_local! {
    /// The tasks running on this thread, innermost last: their `own_thread`.
    static RUNNING: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    /// The task manager made this thread (`thread_start` ran on it).
    static MADE: Cell<bool> = const { Cell::new(false) };
}

/// The threads whose `thread_start` ran, and those whose `thread_end` ran.
static STARTED: AtomicU64 = AtomicU64::new(0);
static ENDED: AtomicU64 = AtomicU64::new(0);

/// The calls of `workers_end`.
static WORKERS_END: AtomicU64 = AtomicU64::new(0);

/// The standard workers whose `thread_start` ran (`sched::running_worker`
/// names one), those whose `thread_end` ran, and the first count when
/// `main` called `finish`: every one of those has ended by `workers_end`. A
/// worker made later (LB-13's corrected run of a task a dedicated task
/// enqueues) may still run then.
static WORKERS_STARTED: AtomicU64 = AtomicU64::new(0);
static WORKERS_ENDED: AtomicU64 = AtomicU64::new(0);
static WORKERS_AT_FINISH: AtomicU64 = AtomicU64::new(0);

/// The tasks open on the calling thread (none once its thread-locals are
/// gone: a hook that runs then checks nothing).
fn open_tasks() -> usize {
    RUNNING.try_with(|r| r.borrow().len()).unwrap_or(0)
}

pub struct DriverGlue;

impl Glue for DriverGlue {
    fn thread_start(&self) {
        assert!(
            !MADE.get(),
            "sched-cases-mt: thread_start twice on a thread"
        );
        assert_eq!(open_tasks(), 0, "sched-cases-mt: thread_start in a task");
        MADE.set(true);
        STARTED.fetch_add(1, Ordering::SeqCst);
        if sched::running_worker().is_some() {
            WORKERS_STARTED.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn thread_end(&self) {
        assert!(
            MADE.get(),
            "sched-cases-mt: thread_end without thread_start"
        );
        assert_eq!(
            open_tasks(),
            0,
            "sched-cases-mt: thread_end with a task open"
        );
        MADE.set(false);
        ENDED.fetch_add(1, Ordering::SeqCst);
        if sched::running_worker().is_some() {
            WORKERS_ENDED.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn task_begin(&self, own_thread: bool) {
        let _ = RUNNING.try_with(|r| {
            let mut r = r.borrow_mut();
            assert!(
                !own_thread || r.is_empty(),
                "sched-cases-mt: a task with a thread of its own began inside another task"
            );
            // and only on a thread the task manager made (a worker, a
            // dedicated task's thread): never on `main` or after `finish`
            // (review RT3-03)
            assert!(
                !own_thread || MADE.get(),
                "sched-cases-mt: a task with a thread of its own began on a thread the manager did not make"
            );
            r.push(own_thread);
        });
    }

    fn task_end(&self, own_thread: bool) {
        let _ = RUNNING.try_with(|r| {
            let begun = r.borrow_mut().pop();
            assert_eq!(
                begun,
                Some(own_thread),
                "sched-cases-mt: task_end without its task_begin"
            );
        });
    }

    fn workers_end(&self) {
        assert!(
            !MADE.get(),
            "sched-cases-mt: workers_end on a thread the manager made"
        );
        assert_eq!(open_tasks(), 0, "sched-cases-mt: workers_end in a task");
        let before = WORKERS_END.fetch_add(1, Ordering::SeqCst);
        assert_eq!(before, 0, "sched-cases-mt: workers_end twice");
        let (at_finish, ended) = (
            WORKERS_AT_FINISH.load(Ordering::SeqCst),
            WORKERS_ENDED.load(Ordering::SeqCst),
        );
        assert!(
            ended >= at_finish,
            "sched-cases-mt: workers_end with {at_finish} workers made before finish, {ended} ended"
        );
    }
}

/// Lean's generated `main`: the initializers, `lean_init_task_manager`,
/// `main`, `lean_finalize_task_manager` (the final run, `sched::finish`),
/// then the flush of the streams at `exit` (decisions Q5 refinement A).
pub fn run(init: impl FnOnce(), main: impl FnOnce(&[String]) -> u32, args: &[String]) -> ! {
    // Lean's stack-overflow report for this thread (the initializers' and
    // `main`'s); every thread the task manager makes installs it itself.
    sched::install_stack_overflow_handler();
    init();
    sched::start(Arc::new(DriverGlue));
    let code = main(args);
    WORKERS_AT_FINISH.store(WORKERS_STARTED.load(Ordering::SeqCst), Ordering::SeqCst);
    sched::finish();
    // The hooks' contract at the end: every thread the task manager made has
    // ended, and no task is open on `main`'s thread.
    let (started, ended) = (STARTED.load(Ordering::SeqCst), ENDED.load(Ordering::SeqCst));
    assert_eq!(
        started, ended,
        "sched-cases-mt: threads started {started}, ended {ended} after finish"
    );
    assert_eq!(open_tasks(), 0, "sched-cases-mt: a task open on main");
    if sched::lean_num_threads() != 0 {
        assert_eq!(
            WORKERS_END.load(Ordering::SeqCst),
            1,
            "sched-cases-mt: finish returned without workers_end"
        );
    }
    exit::exit(code as i32)
}
