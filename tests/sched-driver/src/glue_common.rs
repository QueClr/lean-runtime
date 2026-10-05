//! The glue's parts that do not depend on the scheduler, shared by both
//! drivers (`tests/sched-driver-mt` includes this file by path): the
//! standard streams' output, Lean's panics and `IO.Process.exit` (the
//! crate's executor, `io::panic`, with its native glue), and native Lean's
//! startup descriptors. Each driver's `glue.rs` adds its `Glue` and
//! the program's entry (`run`), and re-exports these.

use lean_runtime::io::panic::{self, Native};
use lean_runtime::io::Handle;
use lean_runtime::sched;

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

/// A Lean panic of the runtime (`lean_panic(msg)`: `Task.get` in a `sync`
/// task): the crate's executor (`io::panic::report`) with native's glue on
/// the crate's streams: on Lean's current stderr, which `IO.setStderr`
/// redirects, unless the process is about to end; the program goes on.
pub fn lean_panic(msg: &str) {
    panic::report(msg.as_bytes(), false, &mut Native)
}

/// `lean_panic(msg, force_stderr = true)`, the report of
/// `IO.Option.getOrBlock!` on `none` (`sched::option_get_or_block`): always
/// on the process's stderr, never on the stream `IO.setStderr` set.
pub fn lean_panic_forced(msg: &str) {
    panic::report(msg.as_bytes(), true, &mut Native)
}

/// The text of the uncaught error a port's `main` ended with
/// ([`uncaught_after_main`]), reported by [`end`].
static UNCAUGHT: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// A port's `main` ends with an uncaught error whose text is `text`
/// (`IO.Error.toString`); it returns status 1. The driver's `run` reports
/// it after `sched::finish` ([`end`]), as Lean's generated `main` calls
/// `lean_io_result_show_error` after `lean_finalize_task_manager` (review
/// RSH3-04). The net ports call it, in both drivers.
pub fn uncaught_after_main(text: &[u8]) {
    *UNCAUGHT.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_vec());
}

/// The end of the driver's `run`, after `sched::finish`: an uncaught error
/// recorded by [`uncaught_after_main`] through the crate's executor
/// (`io::panic::uncaught`: its line, status 1), otherwise C's `exit` with
/// `main`'s status.
pub fn end(code: u32) -> ! {
    let pending = UNCAUGHT.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(text) = pending {
        panic::uncaught(&text, &mut Native)
    }
    lean_runtime::io::exit::exit(code as i32)
}

/// `IO.Process.exit` (`lean_io_exit`): the crate's executor
/// (`io::panic::process_exit`): an effect point, then C's `exit`, which
/// flushes the streams but neither finalizes the task manager nor waits
/// for any task.
pub fn process_exit(code: u8) -> ! {
    panic::process_exit(code, &mut Native)
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
