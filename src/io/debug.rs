//! The IO parts of Lean's debug primitives (`object.cpp`) and the runtime's
//! own standard-error lines.
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
