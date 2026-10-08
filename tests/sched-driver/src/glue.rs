//! The glue a translator writes around `lean_runtime::sched`, as small as it
//! can be: the suspend step (its one `unsafe` block), the opt-in to Lean's
//! stack-overflow report (`sched::install_stack_overflow_handler`), and the
//! program's entry and exit. The entry runs `main` as both translators do
//! (lean2rr's `run_main2`): on a thread of its own with Lean's stack size,
//! or on the process's thread with `LEAN_MAIN_USE_THREAD=0`
//! (`io::startup::run_main`; review RF16-02). The parts that do not depend
//! on the scheduler (output, Lean's panics, `IO.Process.exit`, native's
//! startup descriptors) are in `glue_common.rs`, which the threads-mode
//! driver shares.

pub use crate::glue_common::*;
use lean_runtime::sched::{self, CtxId, Glue, Suspend};
use std::convert::Infallible;
use std::rc::Rc;

pub struct DriverGlue;

impl Glue for DriverGlue {
    fn suspend(&self, s: Suspend<'_>) {
        // SAFETY: lean-runtime calls `suspend` only on a context it started,
        // while that context runs, with that context's own yielder, which
        // corosensei keeps at the base of the context's stack until the
        // context's function returns (lean-runtime docs/sched.md, "Why
        // `Glue::suspend` is sound"). The pointer is not kept.
        unsafe { (*s.yielder()).suspend(()) }
    }

    fn switched(&self, _from: CtxId, _to: CtxId) {}
}

/// Lean's generated `main`, as both translators run it (lean2rr's
/// `run_main2`; review RF16-02): the initializers on the process's thread,
/// then `lean_run_main`'s thread (`io::startup::run_main` with
/// `thread_stack_size()`: 1 GiB, or `LEAN_STACK_SIZE_KB` plus 128 KiB; the
/// process's thread itself with `LEAN_MAIN_USE_THREAD=0`), which runs
/// `lean_init_task_manager`, `main`, `lean_finalize_task_manager` (the
/// final run, `sched::finish`), then the flush of the streams at `exit`
/// (decisions Q5 refinement A). The exit comes from `main`'s thread, so
/// that no thread-local of that thread (the scheduler's state) is dropped
/// before it.
///
/// So the stack-room rule of `wait` sees the stack the translators give
/// `main` (docs/sched.md, "The stack room of a run on the waiter's
/// stack"): `main`'s waits run the awaited task on its stack while it has a
/// native worker's room, as in their programs, and take the context path
/// only on the process's thread (`LEAN_MAIN_USE_THREAD=0`, whose stack is
/// `RLIMIT_STACK`) or under deep recursion.
pub fn run(init: fn(), main: fn(&[String]) -> u32, args: Vec<String>) -> ! {
    // Lean's stack-overflow report, for the initializers' thread.
    sched::install_stack_overflow_handler();
    init();
    let ran = lean_runtime::io::startup::run_main(sched::thread_stack_size(), move || {
        // and for `main`'s thread (natively each Lean thread installs it at
        // its start), with the contexts the scheduler runs on it
        sched::install_stack_overflow_handler();
        main_with_tasks(main, &args)
    });
    match ran {
        Ok(never) => match never {},
        // A Rust panic (not a Lean panic: a bug of a port or of the crate)
        // that unwound out of `main` (`a_rust_panic_in_a_context_goes_on_in_main`,
        // the `adv_*` checks): after the panic hook's report, the status of
        // a Rust program's panic, with the streams written, as lean2rr's
        // `run_main2` ends then.
        Err(_) => lean_runtime::io::exit::exit(101),
    }
}

/// `main`'s thread: the task manager's start, `main`, the final run, and the
/// exit (which never returns).
fn main_with_tasks(main: fn(&[String]) -> u32, args: &[String]) -> Infallible {
    let lazy = std::env::var_os("SCHED_DRIVER_LAZY").is_some();
    if lazy {
        // lean2rr's lazy start (`tests/cases.rs`, `lazy_start_cases`): the
        // scheduler is built at the program's first task, promise,
        // `Std.Sync` object, timer, signal watcher, socket or lookup, which
        // also turns the `ST.Ref` read yields on
        sched::start_lazy(
            Rc::new(DriverGlue),
            sched::lean_num_threads(),
            sched::thread_stack_size(),
        );
        // so that `lazy_start_cases` fails unless the lazy path is taken
        // (review RSH2-12): nothing built before `main`'s first scheduler
        // call
        assert!(
            !sched::sched_started(),
            "sched-driver: the scheduler was built at start_lazy"
        );
    } else {
        sched::start(Rc::new(DriverGlue));
        // The cases are programs that create tasks (decisions Q5 refinement B).
        sched::set_ref_read_yields(true);
    }
    let code = main(args);
    if lazy {
        // every case `lazy_start_cases` runs creates a task or uses another
        // entry point of the scheduler, which built it
        assert!(
            sched::sched_started(),
            "sched-driver: no entry point started the scheduler"
        );
    }
    sched::finish();
    // an uncaught error's line after the task manager's end, or the exit
    end(code)
}
