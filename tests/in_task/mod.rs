//! What the io twins (`tests/io_cases.rs`, `tests/io2_cases.rs`) share in a
//! threads build (feature `threads`, docs/threads.md, T2): each twin runs
//! inside a task, on a worker thread of the task manager, as the program
//!
//! ```lean
//! def main (args : List String) : IO Unit := do
//!   let t ← IO.asTask (twin args)
//!   IO.ofExcept (← IO.wait t)
//! ```
//!
//! runs natively. So the io layer's locks, its per-thread state (the
//! current streams, the modelled `errno`: a worker's own) and its exit
//! paths are checked on a thread that is not `main`'s, against the same
//! expected outcomes, which
//! native gives for such a program too: every twin, the title's included
//! (their outcomes read the arguments' memory and `/proc/self/cmdline`,
//! not the calling thread's name, which libuv's `prctl(PR_SET_NAME)` sets).

#![allow(dead_code)] // each binary uses what it needs

use lean_runtime::sched;
use std::sync::{Arc, Mutex};

/// The glue: nothing to do in threads mode (a task's streams and `errno`
/// are its thread's).
struct TaskGlue;

impl sched::Glue for TaskGlue {}

/// `twin(args)` as an IO task at the default priority (a pool task), which
/// `main` waits for; then the task manager's end (`sched::finish`, which
/// also joins the io layer's own tasks). The twin's result, for `main` to
/// report.
pub fn run<R: Send + 'static>(twin: fn(&[String]) -> R, args: Vec<String>) -> R {
    sched::start(Arc::new(TaskGlue));
    let slot: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    let id = sched::spawn(
        Box::new(move || {
            assert_ne!(sched::thread_number(), 0, "the twin runs on a worker");
            let r = twin(&args);
            *s2.lock().unwrap() = Some(r);
            sched::Outcome::Done
        }),
        0,
        true,
    );
    sched::wait(id);
    sched::finish();
    let r = slot.lock().unwrap().take();
    r.expect("the twin's task ran")
}
