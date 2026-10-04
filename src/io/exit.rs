//! What a native Lean program's exit does with its streams, and the exits
//! Lean offers (`IO.Process.exit`, `IO.Process.forceExit`, an uncaught
//! error).
//!
//! A native program ends through C's `exit` (after `main` returns, and in
//! `lean_io_exit`). Its atexit work, in order:
//! 1. libc++'s `ios_base::Init` destructor flushes `std::cout`, which with
//!    `sync_with_stdio` is `fflush(stdout)`;
//! 2. glibc's `_IO_cleanup`: `_IO_flush_all` writes the pending output of
//!    every `FILE`, newest first (the open handles, then `stderr`, `stdout`,
//!    `stdin`), then `_IO_unbuffer_all` syncs every used buffered stream,
//!    which gives seekable read-ahead back (stdin is left where the program
//!    stopped reading, for the next process).
//!
//! So a handle on `/dev/stdout` prints after `main`'s own buffered output
//! (case `io/exit_flush_order`). The crate cannot register an atexit handler
//! without `unsafe`, so a translator calls [`exit_flush`] on every path that
//! ends the process normally, after it has run and joined the pending tasks
//! (the exit order agreed for `sched`: `main` returns; pending tasks run and
//! join; the streams are flushed; the process exits), or simply calls
//! [`exit`].
//!
//! Sources: lean2rr's `runtime/leanrt/src/io.rs` (`flush_at_exit`) and
//! leanrs's `rt/leanrs_rt/src/io/env.rs` (`uncaught`, `process_force_exit`).

use super::handle::{lock, open_files_newest_first, STDERR, STDIN, STDOUT};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::PoisonError;

/// The streams' part of C's `exit` in a native Lean program (see the module
/// comment): `fflush(stdout)`, then `_IO_flush_all`, then
/// `_IO_unbuffer_all`. Errors are ignored. `_IO_flush_all` waits for each
/// stream's lock; `_IO_unbuffer_all` skips a stream another thread holds (glibc
/// gives it two tries).
pub fn exit_flush() {
    if exiting_without_flush() {
        return;
    }
    let _ = lock(&STDOUT).flush();
    let open = open_files_newest_first();
    for f in open.iter() {
        f.file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exit_flush();
    }
    for s in [&STDERR, &STDOUT, &STDIN] {
        lock(s).exit_flush();
    }
    for f in open.iter() {
        if let Ok(mut g) = f.file.try_lock() {
            g.exit_unbuffer();
        }
    }
    for s in [&STDERR, &STDOUT, &STDIN] {
        if let Ok(mut g) = s.try_lock() {
            g.exit_unbuffer();
        }
    }
}

/// C's `exit(code)` in a native Lean program (`lean_io_exit` for
/// `IO.Process.exit`, and the end of `main`, whose `UInt32` result the OS
/// takes modulo 256): [`exit_flush`], then `std::process::exit`.
pub fn exit(code: i32) -> ! {
    exit_flush();
    std::process::exit(code)
}

/// `IO.Process.forceExit` (`lean_io_force_exit`, `_Exit`): end the process
/// with none of this crate's flushes, so the streams' pending output is lost.
///
/// It is `std::process::exit`, not `_Exit`: safe Rust has no `_Exit`. What
/// that runs and `_Exit` does not (review RIO1-04):
/// - std's own cleanup: Rust's `std::io::stdout` buffer is flushed (this crate
///   does not use it);
/// - glibc's `exit`: the calling thread's `thread_local!` destructors
///   (`__call_tls_dtors`), the `atexit` and `__cxa_atexit` handlers (C++
///   static destructors, libc++'s flush of `std::cout`), and `_IO_cleanup`,
///   which flushes the C stdio `FILE`s of linked C code.
///
/// The crate registers none of these. A translator whose glue registers any,
/// or links C or C++ code that buffers output, and needs `_Exit` exactly,
/// calls `_exit` from its glue.
///
/// Before `std::process::exit`, a process-wide flag is set that makes every
/// later `fclose` (a handle dropped by a thread-local destructor, a translator
/// keeping handles in thread-locals) and [`exit_flush`] discard pending output
/// instead of writing it, as `_Exit` would never have written it (leanrs
/// review F3).
pub fn force_exit(code: i32) -> ! {
    EXITING_WITHOUT_FLUSH.store(true, Ordering::SeqCst);
    std::process::exit(code)
}

/// Set by [`force_exit`]: the process is ending as `_Exit` ends it.
static EXITING_WITHOUT_FLUSH: AtomicBool = AtomicBool::new(false);

/// Whether [`force_exit`] is ending the process: no stream writes its pending
/// output any more.
pub(crate) fn exiting_without_flush() -> bool {
    EXITING_WITHOUT_FLUSH.load(Ordering::SeqCst)
}

/// `lean_io_result_show_error` for an uncaught error whose text is `msg`
/// (`IO.Error.toString`, the translator's): `std::cerr << "uncaught
/// exception: " << msg << std::endl`. `std::cerr` is tied to `std::cout`, so
/// `stdout` is flushed first; then three unbuffered writes to `stderr`, `msg`
/// up to its first NUL byte (Lean prints a C string). Errors are ignored. The
/// translator then exits with status 1 ([`exit`]).
pub fn show_error(msg: &[u8]) {
    let _ = lock(&STDOUT).flush();
    let shown = msg.split(|&b| b == 0).next().unwrap_or(&[]);
    let mut err = lock(&STDERR);
    let _ = err.put(b"uncaught exception: ");
    let _ = err.put(shown);
    let _ = err.put(b"\n");
}
