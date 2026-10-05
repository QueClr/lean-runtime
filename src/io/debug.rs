//! The IO parts of Lean's debug primitives (`object.cpp`), `allocprof`
//! (`io.cpp`) and the runtime's own standard-error lines.
//!
//! Owner's ruling DV5 (decisions.md): `dbgTrace` prints its message to the
//! current standard-error stream and continues, `dbgSleep` sleeps, both as in
//! Lean; `dbgTraceIfShared` prints [`shared_rc_line`] when the translator
//! finds its value shared (sharing depends on its representation);
//! `dbgStackTrace` continues without a trace.
//!
//! Natively the runtime writes these lines with `io_eprintln`, which calls
//! Lean's `IO.eprintln`, so they go to the calling thread's *current*
//! standard-error stream: [`runtime_eprintln`] writes through the stream
//! `IO.setStderr` made current ([`super::streams`]), else to glibc's
//! `stderr`, the stream every thread starts with. A test harness can take
//! the lines that would go to glibc's `stderr` ([`set_stderr_fallback`]).
//!
//! The texts are also functions of their own ([`allocprof_text`],
//! [`shared_rc_line`]), for a glue whose current standard-error stream is
//! its own value (lean2rr): it writes the bytes with that stream's
//! `putStr`.

use std::sync::{PoisonError, RwLock};

use super::handle::{lock, STDERR};

/// A writer for the runtime's own standard-error lines while the calling
/// thread's standard-error stream is the one it started with: it gets the
/// line (its newline included) and returns whether it took it. On `false`
/// the line goes to glibc's `stderr`, as without a hook.
pub type StderrFallback = fn(&[u8]) -> bool;

/// The hook [`set_stderr_fallback`] installed.
static FALLBACK: RwLock<Option<StderrFallback>> = RwLock::new(None);

/// Installs `hook` (or none) for the whole process and returns the hook it
/// replaces. From then on, the runtime's own lines ([`runtime_eprintln`] and
/// what writes through it: [`dbg_trace`], [`dbg_trace_shared`],
/// [`allocprof`], `time::timeit`) go to `hook` first when the calling
/// thread has no standard-error stream of `IO.setStderr`'s
/// ([`super::streams::put_current_stderr`] is false).
///
/// It is for a test harness that captures these lines (leanrs's
/// `proptest`). A program built for use leaves it unset: the lines then go
/// to glibc's `stderr`, as natively. The hook is a plain `fn`, so a capture
/// per thread keeps its buffer in a `thread_local!`. It may write to any
/// stream, but must not call [`set_stderr_fallback`]. It also takes a
/// panic's lines on Lean's stream (`io::panic::report` through the default
/// `PanicGlue::lean_eprintln`), but no other write of the crate: a panic's
/// lines on the process's stderr (`LEAN_ABORT_ON_PANIC`, `force_stderr`),
/// an internal panic, an uncaught error, or the handle of `IO.getStderr`
/// (review RSH3-02).
pub fn set_stderr_fallback(hook: Option<StderrFallback>) -> Option<StderrFallback> {
    let mut slot = FALLBACK.write().unwrap_or_else(PoisonError::into_inner);
    std::mem::replace(&mut *slot, hook)
}

/// `io_eprintln` of a text whose newline is already at its end: the current
/// standard-error stream's `putStr`, else the fallback hook, else glibc's
/// `stderr` (unbuffered, so one `write`). Errors are ignored.
fn put_runtime_line(line: &[u8]) {
    if super::streams::put_current_stderr(line) {
        return;
    }
    let hook = *FALLBACK.read().unwrap_or_else(PoisonError::into_inner);
    if hook.is_some_and(|h| h(line)) {
        return;
    }
    let _ = lock(&STDERR).put(line);
}

/// `io_eprintln(s)` (object.cpp): `IO.eprintln`, one `putStr` of `msg` and
/// `\n` on the calling thread's current standard-error stream
/// ([`super::streams::put_current_stderr`]), or on `stderr` (unbuffered, so
/// one `write`) while the current one is the default (through the hook of
/// [`set_stderr_fallback`] if one is installed). Errors are ignored.
pub fn runtime_eprintln(msg: &[u8]) {
    let mut line = Vec::with_capacity(msg.len() + 1);
    line.extend_from_slice(msg);
    line.push(b'\n');
    put_runtime_line(&line);
}

/// `dbgTrace` (`lean_dbg_trace`): `io_eprintln(msg)`; the translator then
/// applies the continuation.
pub fn dbg_trace(msg: &[u8]) {
    runtime_eprintln(msg)
}

/// What `dbgTraceIfShared` (`lean_dbg_trace_if_shared`) writes when its
/// value is shared: `shared RC `, `msg` up to its first NUL byte (Lean
/// prints `lean_string_cstr(s)`) and `io_eprintln`'s newline. These are the
/// bytes of the one `putStr` on the current standard-error stream.
///
/// Source: lean2rr's `lean_dbg_trace_if_shared` and `l2r_shared_rc_text`
/// (`runtime/prelude.rr`) and leanrs_rt's `dbg_trace_if_shared_rc`
/// (`src/introspect.rs`).
pub fn shared_rc_line(msg: &[u8]) -> Vec<u8> {
    let msg = up_to_nul(msg);
    let mut line = Vec::with_capacity(b"shared RC ".len() + msg.len() + 1);
    line.extend_from_slice(b"shared RC ");
    line.extend_from_slice(msg);
    line.push(b'\n');
    line
}

/// `dbgTraceIfShared` once the translator has found its value shared (not a
/// scalar, and not exclusive): [`shared_rc_line`] written as
/// [`runtime_eprintln`] writes. The test of sharing is the translator's.
pub fn dbg_trace_shared(msg: &[u8]) {
    put_runtime_line(&shared_rc_line(msg));
}

/// `dbgSleep` (`lean_dbg_sleep`): sleep `ms` milliseconds; the translator
/// then applies the continuation.
pub fn dbg_sleep(ms: u32) {
    super::env::sleep(ms)
}

/// What `allocprof` prints after its message: Lean 4.34.0's release runtime
/// is built without `LEAN_RUNTIME_STATS` (`allocprof.cpp`, the `#else`
/// branch of `~allocprof`).
pub const ALLOCPROF_NOTE: &[u8] =
    b"Allocation profiling data is not available, compile lean using `-D RUNTIME_STATS=ON`";

/// `msg` up to its first NUL byte: C's view of a Lean string
/// (`string_cstr`).
pub(crate) fn up_to_nul(msg: &[u8]) -> &[u8] {
    match msg.iter().position(|&b| b == 0) {
        Some(n) => &msg[..n],
        None => msg,
    }
}

/// What `allocprof` writes after its action (`lean_io_allocprof`): `msg` up
/// to its first NUL byte (Lean prints `string_cstr(msg)`), a newline,
/// [`ALLOCPROF_NOTE`], a newline, and `io_eprintln`'s newline. These are the
/// bytes of the one `putStr` on the current standard-error stream.
///
/// Source: lean2rr leanrt `src/io.rs` (`allocprof_text`).
pub fn allocprof_text(msg: &[u8]) -> Vec<u8> {
    let msg = up_to_nul(msg);
    let mut text = Vec::with_capacity(msg.len() + ALLOCPROF_NOTE.len() + 3);
    text.extend_from_slice(msg);
    text.push(b'\n');
    text.extend_from_slice(ALLOCPROF_NOTE);
    text.extend_from_slice(b"\n\n");
    text
}

/// `allocprof msg act` (`lean_io_allocprof`): runs `act`, then writes
/// [`allocprof_text`] as [`runtime_eprintln`] writes, and returns `act`'s
/// result, an error included. The message is printed after `act`, to the
/// standard-error stream current then.
pub fn allocprof<R>(msg: &[u8], act: impl FnOnce() -> R) -> R {
    let r = act();
    put_runtime_line(&allocprof_text(msg));
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        /// The lines the test hook took on this thread, while capturing.
        static TAKEN: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }

    /// Takes the line on a thread that captures, as a harness's hook does;
    /// any other thread's line goes to `stderr`.
    fn capture(line: &[u8]) -> bool {
        TAKEN.with(|t| match &mut *t.borrow_mut() {
            Some(v) => {
                v.extend_from_slice(line);
                true
            }
            None => false,
        })
    }

    #[test]
    fn texts() {
        assert_eq!(shared_rc_line(b"x"), b"shared RC x\n");
        assert_eq!(shared_rc_line(b"a\0b"), b"shared RC a\n");
        assert_eq!(shared_rc_line(b""), b"shared RC \n");
        let mut want = b"m\n".to_vec();
        want.extend_from_slice(ALLOCPROF_NOTE);
        want.extend_from_slice(b"\n\n");
        assert_eq!(allocprof_text(b"m\0hidden"), want);
    }

    /// The hook takes the runtime's lines on the capturing thread, in
    /// order, each with the bytes `io_eprintln` writes; the previous hook
    /// comes back when it is replaced. (The other unit tests run on their
    /// own threads, which do not capture, so their lines still reach
    /// `stderr`.)
    #[test]
    fn fallback_hook_takes_the_runtime_lines() {
        TAKEN.with(|t| *t.borrow_mut() = Some(Vec::new()));
        let before = set_stderr_fallback(Some(capture));
        assert!(before.is_none());
        runtime_eprintln(b"one");
        dbg_trace(b"two");
        dbg_trace_shared(b"three\0x");
        assert_eq!(allocprof(b"four", || 7), 7);
        crate::io::time::timeit(b"five", || ());
        let got = TAKEN.with(|t| t.borrow_mut().take()).unwrap();
        let mut want = b"one\ntwo\nshared RC three\n".to_vec();
        want.extend_from_slice(&allocprof_text(b"four"));
        assert!(
            got.starts_with(&want),
            "{:?}",
            String::from_utf8_lossy(&got)
        );
        let rest = &got[want.len()..];
        assert!(rest.starts_with(b"five ") && rest.ends_with(b"\n"));
        // another thread does not capture: the hook declines its line
        std::thread::spawn(|| assert!(!capture(b"x\n")))
            .join()
            .unwrap();
        let back = set_stderr_fallback(None).expect("the hook was installed");
        assert!(std::ptr::fn_addr_eq(back, capture as StderrFallback));
    }
}
