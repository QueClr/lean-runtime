//! What a native Lean program's exit does with its streams, and the exits
//! Lean offers (`IO.Process.exit`, `IO.Process.forceExit`, an uncaught
//! error).
//!
//! A native program ends through C's `exit` (after `main` returns, and in
//! `lean_io_exit`). Its atexit work, in order:
//! 1. libc++'s `ios_base::Init` destructor flushes `std::cout`, which with
//!    `sync_with_stdio` is `fflush(stdout)`;
//! 2. the program's destructors (`_dl_fini`), among them libuv's
//!    `uv_library_shutdown`, which waits for the name lookups in progress on
//!    its thread pool (`net::dns`, with the feature `net`);
//! 3. glibc's `_IO_cleanup`: `_IO_flush_all` writes the pending output of
//!    every `FILE`, newest first (the open handles, then `stderr`, `stdout`,
//!    `stdin`), then `_IO_unbuffer_all` syncs every used buffered stream,
//!    which gives seekable read-ahead back (stdin is left where the program
//!    stopped reading, for the next process).
//!
//! So a handle on `/dev/stdout` prints after `main`'s own buffered output
//! (case `io/exit_flush_order`).
//!
//! **Streams another context or thread holds** (LB-29, `docs/lean-bugs.md`).
//! glibc 2.39's `fflush` and `_IO_flush_all` take each stream's lock, so the
//! exit waits for whoever holds it. What the holder does decides here (each
//! stream records it under its lock: idle, blocked reading, writing):
//! - a holder blocked **reading** (a task's `readToEnd` of a pipe, `getLine`
//!   on stdin) is not waited for: the stream has nothing to flush (C
//!   requires a flush between output and input, and glibc makes it), and
//!   natively the wait can last for good (`IO.Process.output` of `yes` under
//!   `ulimit -v`: the panic is printed, the process never ends). LB-29;
//!   cases `process/exit_while_reading`, `process/output_oom_both_pipes`,
//!   `process/output_drain_exit_exit` and `_panic`;
//! - a holder **writing** (or holding the stream for anything else) is
//!   waited for, as natively: `exit` flushes every stream with unwritten
//!   data (C11 7.22.4.4, POSIX), and a live reader gets every byte; a reader
//!   that never reads makes the exit wait for good, as natively (cases
//!   `process/exit_while_writing` and `_stalled`). The wait is cooperative
//!   when the holder is a suspended context of this thread (`io::coop`'s
//!   stream locks), so the writing context, and the tasks that drain its
//!   reader, run during the exit, as natively the other threads go on
//!   running during `exit`; the holder being another thread, it is a plain
//!   wait. In a no-suspend scope a suspended holder cannot be waited for,
//!   and its stream is skipped.
//!
//! The crate cannot register an atexit handler
//! without `unsafe`, so a translator calls [`exit_flush`] on every path that
//! ends the process normally, after it has run and joined the pending tasks
//! (the exit order agreed for `sched`: `main` returns; pending tasks run and
//! join, and so do the io layer's own dedicated tasks, [`after_main`]; the
//! streams are flushed; the process exits), or simply calls [`exit`].
//!
//! Sources: lean2rr's `runtime/leanrt/src/io.rs` (`flush_at_exit`) and
//! leanrs's `rt/leanrs_rt/src/io/env.rs` (`uncaught`, `process_force_exit`).

use super::cfile::{std_busy, CFile, BUSY_INPUT};
use super::handle::{lock, open_files_newest_first, try_lock, StreamGuard, STDERR, STDIN, STDOUT};
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The io layer's part of `lean_finalize_task_manager`, which a native
/// program's C `main` calls after the program's `main` has returned,
/// whatever it returned, and before it reports an uncaught error: it waits
/// for the dedicated tasks the crate runs for the program, the
/// standard-output readers that `IO.Process.output` leaves running when it
/// fails on standard error (`process::output`; AR-6). So the process ends at
/// the child's end of file, and the child's writes meanwhile find a reader,
/// as natively (case `process/output_drain_exit`).
///
/// `sched::finish` calls it; a glue without `sched` calls it itself
/// once `main` has returned, before [`show_error`] and [`exit`]. Not on the
/// other ends: `IO.Process.exit` and an internal panic end with C's `exit`,
/// which waits for no task. (Natively that `exit` still waits for the
/// reader, whose `fread` holds its pipe's stream lock: LB-29, not
/// reproduced; cases `process/output_drain_exit_exit` and `_panic`.)
pub fn after_main() {
    super::process::join_drains();
}

/// The lock of a stream for the exit's flush (see the module comment, and
/// LB-29): at once if free; `None` once its holder is blocked reading it
/// (`reading`); otherwise looked at again, every 1 to 16 ms, until it is
/// free or its holder reads, so a holder that takes the lock, or writes, and
/// only then starts a read is seen reading (leanrs's re-check of a771e57,
/// review RFX1-17 (b)). Between looks:
/// - the holder a suspended context of this thread: the other contexts run
///   (it is one of them); in a no-suspend scope, where they cannot, `None`;
/// - the holder another thread: the thread sleeps, also in a no-suspend
///   scope (review RFX1-17 (a));
/// - the holder the exiting context itself (an exit while it holds a
///   stream's guard, which a glue must not do): `None`, the stream's pending
///   output unwritten, where a relock would wait for good (RFX1-17 (c)).
pub(crate) fn exit_lock(m: &Mutex<CFile>, reading: impl Fn() -> bool) -> Option<StreamGuard<'_>> {
    const FIRST: std::time::Duration = std::time::Duration::from_millis(1);
    const MAX: std::time::Duration = std::time::Duration::from_millis(16);
    let mut nap = FIRST;
    loop {
        if let Some(g) = try_lock(m) {
            return Some(g);
        }
        if reading() {
            return None;
        }
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() {
            use super::coop::{holder_of, Holder};
            match holder_of(m) {
                Holder::Me => return None,
                Holder::Suspended if crate::sched::in_no_suspend() => return None,
                Holder::Suspended => {
                    crate::sched::block_until(std::time::Instant::now() + nap);
                    nap = (nap * 2).min(MAX);
                    continue;
                }
                Holder::Other => {}
            }
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(MAX);
    }
}

/// Whether standard stream `n`'s holder is blocked reading it.
fn std_reading(n: u8) -> impl Fn() -> bool {
    move || std_busy(n).load(AtomicOrdering::Acquire) == BUSY_INPUT
}

/// The streams' part of C's `exit` in a native Lean program (see the module
/// comment): the writer threads of the exiting context's dropped streams
/// (feature `sched`; natively done before the exit), then `fflush(stdout)`,
/// then (feature `net`) the name lookups in progress, then
/// `_IO_flush_all`, then `_IO_unbuffer_all`. Errors are ignored. A stream
/// whose holder is blocked reading it is skipped (LB-29); one held otherwise
/// is waited for (cooperatively from a context: the program's other contexts
/// may run meanwhile, as natively its other threads run during `exit`);
/// `_IO_unbuffer_all` skips a held stream, as glibc does after two tries.
pub fn exit_flush() {
    if exiting_without_flush() {
        return;
    }
    // The streams the exiting context handed to writer threads
    // (`io::coop::hand_off`, review RSIO-09; AR-8): natively its drop's
    // `fclose` wrote them before this exit (reviews RFX1-07, RFX1-12).
    // Other contexts' writers are not waited for (RFX1-09).
    #[cfg(feature = "sched")]
    super::coop::join_own_writers(super::coop::JoinAt::Exit);
    if let Some(mut g) = exit_lock(&STDOUT, std_reading(1)) {
        let _ = g.flush();
    }
    #[cfg(feature = "net")]
    crate::net::dns::exit_wait();
    let open = open_files_newest_first();
    for f in open.iter() {
        if let Some(mut g) = exit_lock(&f.file, || f.busy_reading()) {
            g.exit_flush();
        }
    }
    for (n, s) in [(2, &STDERR), (1, &STDOUT), (0, &STDIN)] {
        if let Some(mut g) = exit_lock(s, std_reading(n)) {
            g.exit_flush();
        }
    }
    for f in open.iter() {
        if let Some(mut g) = try_lock(&f.file) {
            g.exit_unbuffer();
        }
    }
    for s in [&STDERR, &STDOUT, &STDIN] {
        if let Some(mut g) = try_lock(s) {
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
///
/// First, the writer threads to which the exiting context's drops handed a
/// stream (`io::coop::hand_off`, review RSIO-09; AR-8) are joined: natively
/// its drop's `fclose` wrote those bytes before any `_Exit`, while the
/// program's other threads ran, so while the scheduler runs other contexts
/// the join lets them run (a task may be the one that drains the pipe). No
/// other buffer is flushed, and other contexts' writers are not waited for.
pub fn force_exit(code: i32) -> ! {
    #[cfg(feature = "sched")]
    super::coop::join_own_writers(super::coop::JoinAt::Exit);
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
