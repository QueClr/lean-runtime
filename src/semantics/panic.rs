//! The texts and exit statuses of Lean 4.34.0's panics (`src/runtime/object.cpp`
//! lines 76-210, `src/runtime/stack_overflow.cpp`, `src/runtime/io.cpp`), as
//! data: what to print, where, and how the process goes on. `io::panic`
//! (feature `io`) carries the plans out, each translator supplying its own
//! streams (`io::panic::PanicGlue`).
//!
//! Two paths:
//! - **`lean_panic_fn`** (`panic!`, `get!` out of bounds, ...): the message
//!   goes to Lean's current stderr stream (`io_eprintln`, so `IO.setStderr`
//!   and `withIsolatedStreams` capture it) as one line, all its bytes, NUL
//!   bytes included (`lean_panic_impl` takes the string's size, not
//!   `strlen`; row `panic/panic.nul`), then, unless
//!   `LEAN_BACKTRACE=0`, `backtrace:` and the frames; the call returns the
//!   default value. With `LEAN_ABORT_ON_PANIC` set (any value) or
//!   exit-on-panic on, the lines go to the process's stderr directly
//!   (`std::cerr`), then the process aborts (status 134) or exits with
//!   status 1. `panic_fn_plan` gives the plan. The runtime's own panics
//!   (`lean_panic(msg, force_stderr)`) take the same path; with
//!   `force_stderr`, which only `IO.Option.getOrBlock!` passes
//!   (`sched::option_get_or_block`), the lines always go to the process's
//!   stderr. `lean_panic_plan` gives both plans.
//! - **`lean_internal_panic`** (the runtime's own limits and failures):
//!   `INTERNAL PANIC: <msg>` and a newline on the C `stderr` saved at startup
//!   (not Lean's stream), then `exit(1)`, which flushes C's `stdout`; under
//!   `LEAN_ABORT_ON_PANIC` an abort instead, status 134, with no flush
//!   (LB-07). `InternalPanic` lists the messages, `internal_panic_plan` the
//!   ending.
//!
//! Source: leanrs_rt `src/panic.rs` (the two paths, the abort flag read from
//! the environment) and lean2rr's `runtime/prelude.rr` (`l2r_panic_text`:
//! the `backtrace:` line, `l2r_panic_code_text`), restated as data from
//! `object.cpp`.

use core::fmt;

/// The prefix of an internal panic's line (`lean_internal_panic`).
pub const INTERNAL_PANIC_PREFIX: &str = "INTERNAL PANIC: ";

/// The exit status after an internal panic, or after a `lean_panic_fn` with
/// exit-on-panic on (`std::exit(1)`).
pub const PANIC_EXIT_STATUS: i32 = 1;

/// The status a shell sees after `abort()` (`LEAN_ABORT_ON_PANIC`, a stack
/// overflow): 128 + `SIGABRT` (6).
pub const ABORT_STATUS: i32 = 134;

/// The line `lean_panic_impl` prints after the message when backtraces are
/// on (`LEAN_BACKTRACE` unset or not `0`); the frames follow, one per line.
pub const BACKTRACE_HEADER: &str = "backtrace:";

/// The line lean2rr prints after `backtrace:` in place of native's frames,
/// which no translator reproduces (their addresses change from run to run);
/// `io::panic` prints it too. Native never prints it after `backtrace:`:
/// it is the text of `print_backtrace`'s `#else` branch, which is dead, since
/// `lean_panic_impl` prints `backtrace:` and calls `print_backtrace` only
/// under `#if LEAN_SUPPORTS_BACKTRACE` (`object.cpp` 179-185; without
/// backtrace support native prints neither line; review RSH3-03).
pub const NO_BACKTRACE: &str = "(stack trace unavailable)";

/// What the stack-overflow handler writes to descriptor 2 before it aborts
/// (`segv_handler`, `stack_overflow.cpp`; status `ABORT_STATUS`).
pub const STACK_OVERFLOW_MESSAGE: &str = "\nStack overflow detected. Aborting.\n";

/// The prefix of the line `lean_io_result_show_error` writes to `std::cerr`
/// (tied to C's `stdout`, so pending output is flushed first) when `main`
/// ends with an uncaught `IO` error (`io.cpp`); the error's text up to its
/// first NUL (`string_cstr`) and a newline follow, and the process exits with
/// status 1 (lean2rr's leanrt `uncaught_exception` does the same).
pub const UNCAUGHT_EXCEPTION_PREFIX: &str = "uncaught exception: ";

/// The internal panics of Lean 4.34.0's runtime that a translator's runtime
/// reproduces (`lean_internal_panic` and its wrappers in `object.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InternalPanic {
    /// `lean_internal_panic_out_of_memory`: a failed allocation, a size of
    /// 2^64 or more (`lean_mk_array`), a capacity of 2^63 or more
    /// (`lean_mk_empty_array_with_capacity`; lifted, LB-37:
    /// `array::empty_with_capacity` reserves nothing instead); here also a
    /// `Nat` or `Int` result above the big-number backend's `MAX_BITS`
    /// (`nat::check_result_bits`).
    OutOfMemory,
    /// `lean_internal_panic_unreachable`.
    Unreachable,
    /// `lean_internal_panic_rc_overflow`: declared in `lean.h` but never
    /// raised in 4.34.0, whose reference counts saturate instead
    /// (`LEAN_RC_STICKY`, `lean.h` 614-622); listed for the message only
    /// (review RS2-07).
    RcOverflow,
    /// `lean_internal_panic_overflow`: an object's byte size above 2^64 - 1
    /// (`lean_usize_mul_checked`, `lean_usize_add_checked`).
    IntegerOverflow,
    /// `lean_sorry`: a `sorry` evaluated at run time.
    Sorry,
    /// `lean_nat_pow` with an exponent of 2^32 or more. Lifted (LB-11):
    /// `semantics::nat::pow` computes the result, and returns this only when
    /// the result is above the backend's `MAX_BITS` (review RS2-03).
    NatPowExponent,
    /// `lean_nat_shiftl` of a nonzero value by 2^32 or more. Lifted (LB-12):
    /// `semantics::nat::shiftl` returns this only for a result above
    /// `MAX_BITS`.
    NatShiftlExponent,
    /// `lean_nat_big_shiftr` of a value of 2^32 bits or more by 2^32 or
    /// more. Lifted (LB-04): `semantics::nat::shiftr` computes the result,
    /// so no rule returns it; listed for the message.
    NatShiftrExponent,
}

impl InternalPanic {
    /// The message after `INTERNAL PANIC: `, as `object.cpp` passes it.
    pub const fn message(self) -> &'static str {
        match self {
            InternalPanic::OutOfMemory => "out of memory",
            InternalPanic::Unreachable => "unreachable code has been reached",
            InternalPanic::RcOverflow => "reference counter overflowed",
            InternalPanic::IntegerOverflow => "integer overflow in runtime computation",
            InternalPanic::Sorry => "executed 'sorry'",
            InternalPanic::NatPowExponent => "Nat.pow exponent is too big",
            InternalPanic::NatShiftlExponent => "Nat.shiftl exponent is too big",
            InternalPanic::NatShiftrExponent => "Nat.shiftr exponent is too big",
        }
    }

    /// The whole line `lean_internal_panic` prints: `INTERNAL PANIC: `, the
    /// message and a newline (`fprintf(g_saved_stderr, "INTERNAL PANIC:
    /// %s\n", msg)`).
    pub fn write_line<W: fmt::Write + ?Sized>(self, out: &mut W) -> fmt::Result {
        out.write_str(INTERNAL_PANIC_PREFIX)?;
        out.write_str(self.message())?;
        out.write_str("\n")
    }
}

/// The settings `lean_panic_impl` and `lean_internal_panic` read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanicSettings {
    /// `LEAN_ABORT_ON_PANIC` is set, to any value (`should_abort_on_panic`).
    pub abort_on_panic: bool,
    /// `LEAN_BACKTRACE` is unset or not exactly `0`.
    pub backtrace: bool,
    /// `lean_set_exit_on_panic(true)` or `Lean.Internal.setExitOnPanic true`
    /// ran (`g_exit_on_panic`, off by default).
    pub exit_on_panic: bool,
    /// `lean_set_panic_messages` has not turned messages off
    /// (`g_panic_messages`, on by default).
    pub messages: bool,
}

impl PanicSettings {
    /// The settings of a process started with the given values of
    /// `LEAN_ABORT_ON_PANIC` and `LEAN_BACKTRACE` (`None` when unset): the
    /// defaults for the two flags no environment variable controls.
    ///
    /// Source: new, from `object.cpp` (`should_abort_on_panic`: set, to any
    /// value; `lean_panic_impl`: `LEAN_BACKTRACE` other than `0`); leanrs_rt
    /// `src/panic.rs` (`abort_on_panic`) and lean2rr's leanrt `panic_msg` read
    /// the same variables.
    pub fn from_env(abort_on_panic: Option<&[u8]>, backtrace: Option<&[u8]>) -> PanicSettings {
        PanicSettings {
            abort_on_panic: abort_on_panic.is_some(),
            backtrace: backtrace != Some(b"0"),
            exit_on_panic: false,
            messages: true,
        }
    }
}

/// Where a panic's lines go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicStream {
    /// Lean's current stderr stream (`io_eprintln`: `IO.setStderr` and
    /// `IO.FS.withIsolatedStreams` redirect it), one write per line.
    LeanStderr,
    /// The process's standard error (`std::cerr`, tied to C's `stdout`, so
    /// pending C `stdout` output is flushed first).
    ProcessStderr,
}

/// How the process goes on after a panic's lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicEnd {
    /// The call returns its default value.
    Return,
    /// `abort()`: status `ABORT_STATUS`, no stream flushed.
    Abort,
    /// `std::exit(1)`: C's streams flushed, status `PANIC_EXIT_STATUS`.
    Exit,
}

/// What `lean_panic_fn` does with its message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanicPlan {
    /// The message line is printed (then the backtrace lines, when
    /// `backtrace`).
    pub print: bool,
    /// Where the lines go.
    pub stream: PanicStream,
    /// `BACKTRACE_HEADER` and the frames follow the message.
    pub backtrace: bool,
    /// What happens next.
    pub end: PanicEnd,
}

/// `lean_panic_fn` (`lean_panic_impl` with `force_stderr = false`): the
/// message, then the backtrace, on Lean's stream unless the process is about
/// to end; then abort, exit or return.
///
/// Source: new, from `object.cpp` (`lean_panic_impl`, `panic_eprintln`);
/// leanrs_rt `src/panic.rs` (`report`) and lean2rr's `l2r_panic_text` make
/// the same decisions.
pub fn panic_fn_plan(s: PanicSettings) -> PanicPlan {
    lean_panic_plan(s, false)
}

/// The runtime's `lean_panic(msg, force_stderr)` (`lean_panic_impl`): as
/// `panic_fn_plan`, but with `force_stderr` the lines always go to the
/// process's stderr (`PanicStream::ProcessStderr`), which `IO.setStderr`
/// does not redirect. Either way nothing is printed while messages are off
/// (`g_panic_messages`). Two calls in Lean 4.34.0:
/// - `Task.get` of an unfinished task in a `sync := true` task
///   (`sched::GET_IN_SYNC_TASK`), without `force_stderr`;
/// - `IO.Option.getOrBlock!` on `none` (`sched::PROMISE_DROPPED`), with
///   `force_stderr`. There `PanicEnd::Return` means the thread then waits
///   forever (`sched::option_get_or_block`).
///
/// Source: new, from `object.cpp` (`lean_panic_impl`, `panic_eprintln`:
/// `force_stderr || g_exit_on_panic || should_abort_on_panic()` picks
/// `std::cerr`) and `io.cpp` (`lean_option_get_or_block`).
pub fn lean_panic_plan(s: PanicSettings, force_stderr: bool) -> PanicPlan {
    let ending = s.exit_on_panic || s.abort_on_panic;
    PanicPlan {
        print: s.messages,
        stream: if ending || force_stderr {
            PanicStream::ProcessStderr
        } else {
            PanicStream::LeanStderr
        },
        backtrace: s.messages && s.backtrace,
        end: if s.abort_on_panic {
            PanicEnd::Abort
        } else if s.exit_on_panic {
            PanicEnd::Exit
        } else {
            PanicEnd::Return
        },
    }
}

/// How `lean_internal_panic` ends after its line (always printed, to the
/// saved C `stderr`): an abort under `LEAN_ABORT_ON_PANIC`, otherwise
/// `exit(1)`.
///
/// Source: new, from `object.cpp` (`lean_internal_panic`); leanrs_rt
/// `src/panic.rs` (`internal_panic`) decides the same.
pub fn internal_panic_end(s: PanicSettings) -> PanicEnd {
    if s.abort_on_panic {
        PanicEnd::Abort
    } else {
        PanicEnd::Exit
    }
}

/// The status a shell sees after a `PanicEnd` that ends the process.
///
/// Source: new (`abort()` is `SIGABRT`; `std::exit(1)`).
pub const fn end_status(end: PanicEnd) -> Option<i32> {
    match end {
        PanicEnd::Return => None,
        PanicEnd::Abort => Some(ABORT_STATUS),
        PanicEnd::Exit => Some(PANIC_EXIT_STATUS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four combinations the rows in `tests/cases/panic` exercise.
    #[test]
    fn plans() {
        let quiet = PanicSettings::from_env(None, Some(b"0"));
        assert_eq!(
            panic_fn_plan(quiet),
            PanicPlan {
                print: true,
                stream: PanicStream::LeanStderr,
                backtrace: false,
                end: PanicEnd::Return
            }
        );
        // `LEAN_ABORT_ON_PANIC` set, even to the empty string
        let abort = PanicSettings::from_env(Some(b""), None);
        let plan = panic_fn_plan(abort);
        assert_eq!(
            (plan.stream, plan.backtrace, plan.end),
            (PanicStream::ProcessStderr, true, PanicEnd::Abort)
        );
        assert_eq!(end_status(plan.end), Some(134));
        assert_eq!(internal_panic_end(quiet), PanicEnd::Exit);
        assert_eq!(end_status(internal_panic_end(quiet)), Some(1));
        assert_eq!(internal_panic_end(abort), PanicEnd::Abort);
        let exit = PanicSettings {
            exit_on_panic: true,
            ..quiet
        };
        assert_eq!(panic_fn_plan(exit).end, PanicEnd::Exit);
        assert_eq!(panic_fn_plan(exit).stream, PanicStream::ProcessStderr);
        let mut line = String::new();
        InternalPanic::IntegerOverflow
            .write_line(&mut line)
            .unwrap();
        assert_eq!(
            line,
            "INTERNAL PANIC: integer overflow in runtime computation\n"
        );
    }

    /// `force_stderr` (`IO.Option.getOrBlock!`): the process's stderr in
    /// every case, the rest as without it; nothing when messages are off.
    #[test]
    fn forced_plans() {
        let quiet = PanicSettings::from_env(None, Some(b"0"));
        assert_eq!(
            lean_panic_plan(quiet, true),
            PanicPlan {
                print: true,
                stream: PanicStream::ProcessStderr,
                backtrace: false,
                end: PanicEnd::Return
            }
        );
        let abort = PanicSettings::from_env(Some(b"1"), None);
        assert_eq!(
            lean_panic_plan(abort, true),
            PanicPlan {
                print: true,
                stream: PanicStream::ProcessStderr,
                backtrace: true,
                end: PanicEnd::Abort
            }
        );
        let silent = PanicSettings {
            messages: false,
            ..quiet
        };
        let plan = lean_panic_plan(silent, true);
        assert_eq!(
            (plan.print, plan.backtrace, plan.end),
            (false, false, PanicEnd::Return)
        );
        for s in [quiet, abort, silent] {
            assert_eq!(lean_panic_plan(s, false), panic_fn_plan(s));
        }
    }
}
