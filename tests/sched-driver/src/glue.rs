//! The glue a translator writes around `lean_runtime::sched`, as small as it
//! can be: the suspend step (its one `unsafe` block), the opt-in to Lean's
//! stack-overflow report (`sched::install_stack_overflow_handler`), and the
//! program's entry and exit. The parts that do not depend on the scheduler
//! (output, Lean's panics, `IO.Process.exit`, native's startup descriptors)
//! are in `glue_common.rs`, which the threads-mode driver shares.

pub use crate::glue_common::*;
use lean_runtime::io::exit;
use lean_runtime::sched::{self, CtxId, Glue, Suspend};
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

/// Lean's generated `main`: the initializers, `lean_init_task_manager`,
/// `main`, `lean_finalize_task_manager` (the final run, `sched::finish`),
/// then the flush of the streams at `exit` (decisions Q5 refinement A).
pub fn run(init: impl FnOnce(), main: impl FnOnce(&[String]) -> u32, args: &[String]) -> ! {
    // Lean's stack-overflow report, for this thread (the initializers' and
    // `main`'s) and the contexts the scheduler runs on it.
    sched::install_stack_overflow_handler();
    init();
    sched::start(Rc::new(DriverGlue));
    // The cases are programs that create tasks (decisions Q5 refinement B).
    sched::set_ref_read_yields(true);
    let code = main(args);
    sched::finish();
    exit::exit(code as i32)
}
