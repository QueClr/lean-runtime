//! The IO parts of Lean's debug primitives (`object.cpp`), `allocprof`
//! (`io.cpp`) and the runtime's own standard-error lines.
//!
//! Owner's ruling DV5 (decisions.md): `dbgTrace` prints its message to the
//! current standard-error stream and continues, `dbgSleep` sleeps, both as in
//! Lean; `dbgTraceIfShared` is each translator's (sharing depends on its
//! representation); `dbgStackTrace` continues without a trace.
//!
//! Natively the runtime writes these lines with `io_eprintln`, which calls
//! Lean's `IO.eprintln`, so they go to the calling thread's *current*
//! standard-error stream: [`runtime_eprintln`] writes through the stream
//! `IO.setStderr` made current ([`super::streams`]), else to glibc's
//! `stderr`, the stream every thread starts with.

use super::handle::{lock, STDERR};

/// `io_eprintln(s)` (object.cpp): `IO.eprintln`, one `putStr` of `msg` and
/// `\n` on the calling thread's current standard-error stream
/// ([`super::streams::put_current_stderr`]), or on `stderr` (unbuffered, so
/// one `write`) while the current one is the default. Errors are ignored.
pub fn runtime_eprintln(msg: &[u8]) {
    let mut line = Vec::with_capacity(msg.len() + 1);
    line.extend_from_slice(msg);
    line.push(b'\n');
    if !super::streams::put_current_stderr(&line) {
        let _ = lock(&STDERR).put(&line);
    }
}

/// `dbgTrace` (`lean_dbg_trace`): `io_eprintln(msg)`; the translator then
/// applies the continuation.
pub fn dbg_trace(msg: &[u8]) {
    runtime_eprintln(msg)
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

/// `allocprof msg act` (`lean_io_allocprof`): runs `act`, then writes `msg` up
/// to its first NUL byte (Lean prints `string_cstr(msg)`), a newline,
/// [`ALLOCPROF_NOTE`] and a newline through [`runtime_eprintln`] (one more
/// newline), and returns `act`'s result, an error included. The message is
/// printed after `act`, to the standard-error stream current then.
pub fn allocprof<R>(msg: &[u8], act: impl FnOnce() -> R) -> R {
    let r = act();
    let msg = match msg.iter().position(|&b| b == 0) {
        Some(n) => &msg[..n],
        None => msg,
    };
    let mut text = Vec::with_capacity(msg.len() + ALLOCPROF_NOTE.len() + 2);
    text.extend_from_slice(msg);
    text.push(b'\n');
    text.extend_from_slice(ALLOCPROF_NOTE);
    text.push(b'\n');
    runtime_eprintln(&text);
    r
}
