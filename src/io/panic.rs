//! The panic and exit executor (redundancy audit item 3.4): what a native
//! Lean 4.34.0 program does, in order, when it panics (`lean_panic_fn`, the
//! runtime's `lean_panic`), when its runtime panics (`lean_internal_panic`),
//! when `main` or an initializer ends with an uncaught error
//! (`lean_io_result_show_error`) and on `IO.Process.exit` (`lean_io_exit`).
//! `semantics::panic` holds the texts and the plans as data; this module
//! carries a plan out: the effect point, the stream, the flush of stdout,
//! the write, then the abort or the exit.
//!
//! Each translator keeps its own streams, so it supplies them through a
//! [`PanicGlue`], whose every method has native's behaviour on the crate's
//! own streams as its default ([`Native`]). A glue overrides only what it
//! keeps of its own: Lean's current stderr stream (lean2rr's stream cells),
//! the settings (leanrs's cached abort flag and its "no backtrace", until
//! leanrs decides; `docs/panic.md`), or its ways to the process's stderr
//! and out of the process (leanrs's test capture).
//!
//! Native's order, path by path (`object.cpp` 76-191, `io.cpp` 62-68 and
//! 1606-1608; probe in `docs/panic.md`):
//! - **[`report`]** (`lean_panic_impl`): with messages on, the message and,
//!   unless `LEAN_BACKTRACE=0`, `backtrace:` and the frames, one line each,
//!   on Lean's current stderr stream (`io_eprintln`, one `putStr` per line,
//!   no flush of stdout); when the process is about to end
//!   (`LEAN_ABORT_ON_PANIC`, exit-on-panic) or forced, on `std::cerr`
//!   instead, which is tied to `std::cout`: C's `stdout` is flushed first.
//!   Then `abort()` (status 134, nothing flushed), `exit(1)`, or the call
//!   returns.
//! - **[`internal_panic`]** (`lean_internal_panic`): `INTERNAL PANIC: `, the
//!   message up to its first NUL byte (`%s`) and a newline, with `fprintf`
//!   on C's `stderr` (unbuffered: one `write`, under the `FILE` lock, so
//!   after another thread's write in progress; stdout is not flushed), then
//!   `abort()` under `LEAN_ABORT_ON_PANIC` (stdout's pending bytes are lost,
//!   LB-07), else `exit(1)` (they are written after the line).
//! - **[`uncaught`]**: `lean_finalize_task_manager`, then
//!   `std::cerr << "uncaught exception: " << msg << std::endl` (stdout
//!   flushed first; the message up to its first NUL byte), then status 1.
//! - **[`process_exit`]** (`lean_io_exit`): C's `exit(code)`.
//!
//! Here a panic and `IO.Process.exit` first make an effect point of the
//! scheduler (natively the other threads run meanwhile, so what they would
//! have printed by now comes first; `sched::effect`). The uncaught error
//! needs none: the translator has run its tasks to their end before. The
//! internal panic runs no other code: it is also the out-of-memory end, and
//! it allocates nothing until its line is written (the line is built on the
//! stack).
//!
//! Source: lean2rr's leanrt `lib.rs` (`panic_text`, `lean_panic`,
//! `internal_panic`, `uncaught_exception`) and `prelude.rr`
//! (`l2r_process_exit`); leanrs_rt `src/panic.rs` (`report_plan`,
//! `internal_panic`, the stack-built line of `io.rs` `write_err_line`) and
//! `src/io/env.rs` (`uncaught`, `process_exit`); the crate's test drivers'
//! `glue_common.rs`, which now call this module.

use core::fmt;

use super::Handle;
use crate::semantics::panic::{
    self as rules, PanicEnd, PanicPlan, PanicSettings, PanicStream, BACKTRACE_HEADER,
    INTERNAL_PANIC_PREFIX, NO_BACKTRACE, PANIC_EXIT_STATUS, UNCAUGHT_EXCEPTION_PREFIX,
};

/// Whether `LEAN_ABORT_ON_PANIC` is set (to any value, also empty), read
/// now: native's `should_abort_on_panic`, a `getenv` at every panic and
/// internal panic, never cached (judge's verdict on the audit's divergence
/// 3), so a program that sets or unsets it with `osSetenv` sees the change
/// at its next panic.
///
/// Allocates nothing while the variable is unset or empty; otherwise std's
/// copy of its value (`std::env::var_os`, the only safe read of the
/// environment). The internal panic reads it after its line is written.
pub fn abort_on_panic() -> bool {
    std::env::var_os("LEAN_ABORT_ON_PANIC").is_some()
}

/// The settings `lean_panic_impl` reads, now: `LEAN_ABORT_ON_PANIC` and
/// `LEAN_BACKTRACE` (each a `getenv` at every panic, natively), with
/// exit-on-panic off and messages on, which no program over `Init` and `Std`
/// can change (`Lean.Internal.setExitOnPanic` is in the `Lean` package,
/// `lean_set_panic_messages` is C only, and Lean 4.34.0's generated `main`
/// leaves messages on during initialization).
pub fn settings() -> PanicSettings {
    let abort = std::env::var_os("LEAN_ABORT_ON_PANIC");
    let backtrace = std::env::var_os("LEAN_BACKTRACE");
    PanicSettings::from_env(
        abort.as_ref().map(|v| v.as_encoded_bytes()),
        backtrace.as_ref().map(|v| v.as_encoded_bytes()),
    )
}

/// What a translator supplies to this module: its streams, its settings and
/// its ways out of the process. Every method has native's behaviour on the
/// crate's own streams as its default, so `Native` implements none, and a
/// glue overrides only what it keeps of its own. Each method that a
/// difference of leanrs's or lean2rr's depends on is named in
/// `docs/panic.md` (its "knob").
pub trait PanicGlue {
    /// `io_eprintln(line)`: `line` and a newline, with one `putStr` of Lean's
    /// current stderr stream (`IO.setStderr` and `withIsolatedStreams`
    /// redirect it). Called once per line of a panic that goes on. Default:
    /// [`super::debug::runtime_eprintln`], through the crate's current
    /// streams ([`super::streams`]).
    fn lean_eprintln(&mut self, line: &[u8]) {
        super::debug::runtime_eprintln(line)
    }

    /// The settings at this panic ([`report`]). Default: [`settings`], the
    /// environment read now.
    fn settings(&mut self) -> PanicSettings {
        settings()
    }

    /// The effect point before a panic's lines, or before its end when it
    /// prints nothing ([`report`]), given the plan. Default: the scheduler's
    /// effect point (`sched::effect`), whatever the stream.
    fn panic_effect(&mut self, plan: PanicPlan) {
        let _ = plan;
        effect()
    }

    /// Whether this internal panic aborts ([`internal_panic`]); it must not
    /// allocate, at least while the variable is unset. Default:
    /// [`abort_on_panic`], the environment read now.
    fn abort_on_panic(&mut self) -> bool {
        abort_on_panic()
    }

    /// `fflush(stdout)`, which `std::cerr`'s tie makes before each write to
    /// the process's stderr. Errors are ignored. Default: the crate's model
    /// of glibc's `stdout`.
    fn flush_stdout(&mut self) {
        let _ = Handle::stdout().flush();
    }

    /// Bytes to the process's stderr (`std::cerr`, C's `stderr`), never to
    /// the stream `IO.setStderr` set. Errors are ignored. Default: the
    /// crate's model of glibc's `stderr` (unbuffered: one `write`).
    fn process_stderr(&mut self, bytes: &[u8]) {
        let _ = Handle::stderr().put_str(bytes);
    }

    /// An internal panic's line to the process's stderr: `line.pieces(out)`
    /// hands its bytes to `out` in order, built on the stack (one piece
    /// unless the line is longer than 256 bytes; each piece ends on a
    /// character boundary). It must not allocate: it is the out-of-memory
    /// end. Errors are ignored. Default: [`write_internal_line_locked`].
    fn internal_stderr(&mut self, line: InternalLine<'_>) {
        write_internal_line_locked(line)
    }

    /// `abort()`: status 134, no stream flushed. Default:
    /// `std::process::abort`.
    fn abort(&mut self) -> ! {
        std::process::abort()
    }

    /// C's `exit(code)`: the streams' pending output written, then the
    /// process ends, waiting for no task. Default: [`super::exit::exit`].
    fn exit(&mut self, code: i32) -> ! {
        super::exit::exit(code)
    }
}

/// Native's glue on the crate's own streams: every [`PanicGlue`] default.
/// The crate's own callers use it (the output drain's out-of-memory end, the
/// test drivers), and so may a translator for a path it keeps nothing of its
/// own on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

impl PanicGlue for Native {}

/// An effect point of the scheduler, where there is one: what natively would
/// have run on other threads by now goes first (`sched::effect`; nothing in
/// threads mode, where they run, or without a scheduler).
#[inline]
fn effect() {
    #[cfg(any(feature = "sched", feature = "threads"))]
    crate::sched::effect();
}

/// `lean_panic_impl(msg, force_stderr)`: the runtime's `lean_panic(msg,
/// force_stderr)` and, with `force_stderr` false, `lean_panic_fn` (Lean's
/// `panic!`, `get!` out of bounds; Lean has formatted `msg` already, as
/// `PANIC at ...`). By the plan `semantics::panic::lean_panic_plan(
/// glue.settings(), force_stderr)`:
/// 1. an effect point ([`PanicGlue::panic_effect`]), if the plan prints or
///    ends the process;
/// 2. with messages on, the lines: `msg` (all its bytes, NUL bytes
///    included, as `lean_panic_impl` takes the string's size), then, with
///    backtraces on, `backtrace:` and `(stack trace unavailable)`, the
///    stand-in lean2rr took for native's frames, which no translator can
///    print (their addresses change from run to run; native never prints
///    that line after `backtrace:`, `semantics::panic::NO_BACKTRACE`);
///    - on Lean's current stderr stream, one [`PanicGlue::lean_eprintln`]
///      per line, as `io_eprintln` makes one `putStr` per line;
///    - or, when the plan ends the process or `force_stderr` is set, on the
///      process's stderr: stdout flushed first, then each line and its
///      newline, as `std::cerr.write(line, size) << "\n"`;
/// 3. `abort()` (`LEAN_ABORT_ON_PANIC`), `exit(1)` (exit-on-panic), or a
///    return: the caller then returns the default value (or, for
///    `IO.Option.getOrBlock!`, blocks; `sched::option_get_or_block`).
///
/// Source: lean2rr's leanrt `panic_text` and `lean_panic`; leanrs_rt
/// `panic.rs` `report_plan`; the drivers' `report_panic`.
#[cold]
#[inline(never)]
pub fn report<G: PanicGlue + ?Sized>(msg: &[u8], force_stderr: bool, glue: &mut G) {
    let plan = rules::lean_panic_plan(glue.settings(), force_stderr);
    if plan.print || plan.end != PanicEnd::Return {
        glue.panic_effect(plan);
    }
    if plan.print {
        let all: [&[u8]; 3] = [msg, BACKTRACE_HEADER.as_bytes(), NO_BACKTRACE.as_bytes()];
        let lines = if plan.backtrace { &all[..] } else { &all[..1] };
        match plan.stream {
            PanicStream::LeanStderr => {
                for l in lines {
                    glue.lean_eprintln(l);
                }
            }
            PanicStream::ProcessStderr => {
                glue.flush_stdout();
                for l in lines {
                    glue.process_stderr(l);
                    glue.process_stderr(b"\n");
                }
            }
        }
    }
    match plan.end {
        PanicEnd::Return => {}
        PanicEnd::Abort => glue.abort(),
        PanicEnd::Exit => glue.exit(PANIC_EXIT_STATUS),
    }
}

/// `lean_internal_panic(msg)`: `INTERNAL PANIC: `, `msg` up to its first NUL
/// byte (`%s`) and a newline, built on the stack and handed to
/// [`PanicGlue::internal_stderr`] (an [`InternalLine`]); then `abort()` if
/// [`PanicGlue::abort_on_panic`], else `exit(1)`. Stdout is not flushed
/// before the line (C's `stderr` has no tie), so with `exit(1)` its pending
/// bytes follow the line, and with the abort they are lost (LB-07).
///
/// No effect point: no other context runs before the line, and nothing is
/// allocated until it is written (with [`Native`], nothing at all while
/// `LEAN_ABORT_ON_PANIC` is unset or empty, until `exit`'s flushes). The
/// messages of `semantics::panic::InternalPanic` are its usual `msg`
/// (`InternalPanic::message`).
///
/// Source: leanrs_rt `panic.rs` `internal_panic` (with `io.rs`
/// `write_err_line`'s stack line); lean2rr's leanrt `internal_panic`; the
/// crate's own copies (the output drain's end, `io::startup`'s line).
#[cold]
#[inline(never)]
pub fn internal_panic<G: PanicGlue + ?Sized>(msg: &str, glue: &mut G) -> ! {
    let shown = match msg.find('\0') {
        Some(n) => &msg[..n],
        None => msg,
    };
    glue.internal_stderr(InternalLine { message: shown });
    if glue.abort_on_panic() {
        glue.abort()
    } else {
        glue.exit(PANIC_EXIT_STATUS)
    }
}

/// An uncaught `IO` error at the top level, of `main` or of an initializer,
/// whose text is `msg` (`IO.Error.toString`, the translator's): as a native
/// program's C `main`, the io layer's dedicated tasks are waited for
/// ([`super::exit::after_main`], its part of `lean_finalize_task_manager`;
/// the translator has run and joined its tasks before, `sched::finish`),
/// then `lean_io_result_show_error`'s `std::cerr << "uncaught exception: "
/// << msg << std::endl`: stdout flushed, then three writes to the process's
/// stderr, `msg` up to its first NUL byte (Lean prints a C string); then
/// status 1, through `exit`. No effect point: no task is left.
///
/// Source: lean2rr's leanrt `uncaught_exception`; leanrs_rt `io/env.rs`
/// `uncaught`.
#[cold]
#[inline(never)]
pub fn uncaught<G: PanicGlue + ?Sized>(msg: &[u8], glue: &mut G) -> ! {
    super::exit::after_main();
    show_error(msg, glue);
    glue.exit(PANIC_EXIT_STATUS)
}

/// `lean_io_result_show_error`'s line ([`uncaught`]), and the crate's
/// [`super::exit::show_error`].
pub(crate) fn show_error<G: PanicGlue + ?Sized>(msg: &[u8], glue: &mut G) {
    glue.flush_stdout();
    glue.process_stderr(UNCAUGHT_EXCEPTION_PREFIX.as_bytes());
    glue.process_stderr(super::debug::up_to_nul(msg));
    glue.process_stderr(b"\n");
}

/// `IO.Process.exit code` (`lean_io_exit`, C's `exit`): an effect point
/// (the exit's flushes are output), then [`PanicGlue::exit`] with `code`,
/// which waits for no task. `LEAN_ABORT_ON_PANIC` plays no part.
///
/// Source: lean2rr's `l2r_process_exit` (`prelude.rr`); leanrs_rt
/// `io/env.rs` `process_exit`; the drivers' `process_exit`.
#[cold]
#[inline(never)]
pub fn process_exit<G: PanicGlue + ?Sized>(code: u8, glue: &mut G) -> ! {
    effect();
    glue.exit(i32::from(code))
}

/// An internal panic's line (`INTERNAL PANIC: `, the message up to its
/// first NUL byte, a newline), for [`PanicGlue::internal_stderr`].
#[derive(Clone, Copy, Debug)]
pub struct InternalLine<'a> {
    message: &'a str,
}

impl InternalLine<'_> {
    /// The message after `INTERNAL PANIC: ` (no NUL byte).
    pub fn message(&self) -> &str {
        self.message
    }

    /// Hands the line's bytes to `out` in order, built in a stack buffer:
    /// one piece, or pieces of at most 256 bytes for a longer line, each
    /// ending on a character boundary. Allocates nothing.
    pub fn pieces(self, out: &mut dyn FnMut(&[u8])) {
        write_internal_line(|w| w.write_str(self.message), out)
    }
}

/// [`PanicGlue::internal_stderr`]'s default: the line's pieces straight to
/// descriptor 2 ([`write_stderr_fd`]), under the lock of the crate's
/// `stderr` model, as `fprintf` on C's unbuffered `stderr` writes under
/// its `FILE` lock: the line waits for another thread's write to `stderr`
/// in progress, and never lands inside it (review RSH3-01). Without the
/// lock when this thread holds it already (glibc's lock is recursive; this
/// one is not, so taking it would wait for good), and, with `sched`, once
/// the program has a task, a promise, a timer or a watch (the cooperative
/// lock allocates and may switch contexts): there the line may land inside
/// another context's or thread's write in progress, a deviation
/// (docs/panic.md, row 10). Allocates nothing.
pub fn write_internal_line_locked(line: InternalLine<'_>) {
    let _held = super::handle::stderr_for_internal_panic();
    line.pieces(&mut write_stderr_fd)
}

/// `bytes` to descriptor 2 at once, in as many `write`s as it takes
/// (`EINTR` retried), with no lock and no allocation; errors (a closed
/// descriptor 2) are ignored.
pub fn write_stderr_fd(bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        match rustix::io::write(rustix::stdio::stderr(), rest) {
            Ok(0) => break,
            Ok(n) => rest = &rest[n..],
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => break,
        }
    }
}

/// The size of the stack buffer an internal panic's line is built in; a
/// longer line goes out in pieces of at most this size (glibc's `vfprintf`
/// on an unbuffered stream does the same with `BUFSIZ`).
const LINE_PIECE: usize = 256;

/// The line of an internal panic: `INTERNAL PANIC: `, what `message` writes,
/// and a newline, built in a stack buffer and handed to `out` in pieces of
/// at most [`LINE_PIECE`] bytes, each ending on a character boundary (one
/// piece for every message the runtimes have; review LS3-01: a capture that
/// decodes each write sees no split character). Allocates nothing (AR-36:
/// `io::startup` builds its line with it in an ELF constructor).
pub(crate) fn write_internal_line(
    message: impl FnOnce(&mut dyn fmt::Write) -> fmt::Result,
    out: &mut dyn FnMut(&[u8]),
) {
    let mut line = Pieces {
        buf: [0; LINE_PIECE],
        len: 0,
        out,
    };
    let _ = fmt::Write::write_str(&mut line, INTERNAL_PANIC_PREFIX);
    let _ = message(&mut line);
    let _ = fmt::Write::write_str(&mut line, "\n");
    line.flush();
}

/// A stack buffer that hands its bytes to `out` whenever it is full, and at
/// the end ([`Pieces::flush`]).
struct Pieces<'a> {
    buf: [u8; LINE_PIECE],
    len: usize,
    out: &'a mut dyn FnMut(&[u8]),
}

impl Pieces<'_> {
    fn flush(&mut self) {
        if self.len > 0 {
            (self.out)(&self.buf[..self.len]);
            self.len = 0;
        }
    }
}

impl fmt::Write for Pieces<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut rest = s;
        while !rest.is_empty() {
            // the most of `rest` that fits, cut at a character boundary
            let mut n = rest.len().min(self.buf.len() - self.len);
            while !rest.is_char_boundary(n) {
                n -= 1;
            }
            if n == 0 {
                // not even the next character fits: a new piece (an empty
                // buffer holds any character)
                self.flush();
                continue;
            }
            self.buf[self.len..self.len + n].copy_from_slice(&rest.as_bytes()[..n]);
            self.len += n;
            rest = &rest[n..];
            if self.len == self.buf.len() {
                self.flush();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "panic_tests.rs"]
mod tests;
