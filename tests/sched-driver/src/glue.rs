//! The glue a translator writes around `lean_runtime::sched`, as small as it
//! can be: the suspend step (its one `unsafe` block), the opt-in to Lean's
//! stack-overflow report (`sched::install_stack_overflow_handler`), and the
//! program's entry and exit.

use lean_runtime::io::{debug, exit, Handle};
use lean_runtime::sched::{self, CtxId, Glue, Suspend};
use lean_runtime::semantics::panic::{self, PanicEnd, PanicSettings, PanicStream};
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

// ---------------------------------------------------------------------------
// Standard streams: the crate's glibc `FILE` model (`lean_runtime::io`):
// stdout fully buffered on the case runner's pipe, stderr unbuffered.

/// `IO.println`: an effect point, then one `putStr` of the line and `\n` on
/// stdout (Lean's `putStrLn`).
pub fn println(s: &str) {
    sched::effect();
    let mut l = String::with_capacity(s.len() + 1);
    l.push_str(s);
    l.push('\n');
    let _ = Handle::stdout().put_str(l.as_bytes());
}

/// `IO.eprintln`: an effect point, then one `putStr` on stderr.
pub fn eprintln(s: &str) {
    sched::effect();
    let mut l = String::with_capacity(s.len() + 1);
    l.push_str(s);
    l.push('\n');
    let _ = Handle::stderr().put_str(l.as_bytes());
}

/// The settings `lean_panic_impl` reads (`LEAN_ABORT_ON_PANIC`,
/// `LEAN_BACKTRACE`; exit-on-panic off and messages on, as in a program
/// after its initializers).
fn panic_settings() -> PanicSettings {
    let abort = std::env::var_os("LEAN_ABORT_ON_PANIC");
    let backtrace = std::env::var_os("LEAN_BACKTRACE");
    PanicSettings::from_env(
        abort.as_ref().map(|v| v.as_encoded_bytes()),
        backtrace.as_ref().map(|v| v.as_encoded_bytes()),
    )
}

/// The runtime's `lean_panic(msg, force_stderr)`, by
/// `lean_panic_plan(settings, force_stderr)`: an effect point, as for any
/// output; the lines on Lean's current stderr (`io_eprintln`, which
/// `IO.setStderr` redirects: `io::debug::runtime_eprintln`) or on the
/// process's stderr (`std::cerr`: C's `stdout` flushed first, then
/// `stderr`, whatever `IO.setStderr` set); then the abort or the exit the
/// plan says; otherwise it returns. No backtrace frames: the frame line of
/// a runtime without backtraces (`NO_BACKTRACE`).
fn report_panic(msg: &str, force_stderr: bool) {
    let plan = panic::lean_panic_plan(panic_settings(), force_stderr);
    if plan.print {
        sched::effect();
        let mut lines = vec![msg];
        if plan.backtrace {
            lines.extend([panic::BACKTRACE_HEADER, panic::NO_BACKTRACE]);
        }
        match plan.stream {
            PanicStream::LeanStderr => {
                for l in lines {
                    debug::runtime_eprintln(l.as_bytes());
                }
            }
            PanicStream::ProcessStderr => {
                let _ = Handle::stdout().flush();
                let err = Handle::stderr();
                for l in lines {
                    let _ = err.put_str(l.as_bytes());
                    let _ = err.put_str(b"\n");
                }
            }
        }
    }
    match plan.end {
        PanicEnd::Abort => std::process::abort(),
        PanicEnd::Exit => exit::exit(panic::PANIC_EXIT_STATUS),
        PanicEnd::Return => {}
    }
}

/// A Lean panic of the runtime (`lean_panic(msg)`: `Task.get` in a `sync`
/// task): on Lean's current stderr, which `IO.setStderr` redirects, unless
/// the process is about to end; the program goes on.
pub fn lean_panic(msg: &str) {
    report_panic(msg, false)
}

/// `lean_panic(msg, force_stderr = true)`, the report of
/// `IO.Option.getOrBlock!` on `none` (`sched::option_get_or_block`): always
/// on the process's stderr, never on the stream `IO.setStderr` set.
pub fn lean_panic_forced(msg: &str) {
    report_panic(msg, true)
}

/// `IO.Process.exit` (`lean_io_exit`): an effect point, then C's `exit`,
/// which flushes the streams but neither finalizes the task manager nor
/// waits for any task.
pub fn process_exit(code: u8) -> ! {
    sched::effect();
    exit::exit(code as i32)
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

// ---------------------------------------------------------------------------
// Native Lean's startup descriptors (libuv's loop: `io::startup`), opened by
// an ELF constructor before Rust's runtime puts `/dev/null` in the place of
// closed standard descriptors, as a translator's glue does; the event loop
// and the signal watchers use them, as natively.

extern "C" fn open_startup_descriptors() {
    if let Err(f) = lean_runtime::io::startup::open_native_descriptors() {
        lean_runtime::io::startup::fail_as_native(f);
    }
}

#[used]
#[link_section = ".init_array"]
static STARTUP: extern "C" fn() = open_startup_descriptors;
