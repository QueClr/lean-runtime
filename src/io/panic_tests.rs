//! Unit tests of `io::panic`: the order of each path's steps, recorded by a
//! glue whose ways out unwind instead of ending the process. The child
//! processes that end for real, with the crate's own streams, are in
//! `tests/io_panic.rs`.

use super::*;
use crate::semantics::panic::InternalPanic;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

/// What a glue was asked to do, in order.
#[derive(Debug, PartialEq, Eq)]
enum Call {
    Effect(PanicStream),
    Lean(Vec<u8>),
    Flush,
    Stderr(Vec<u8>),
    Internal(Vec<u8>),
    Abort,
    Exit(i32),
}

use Call::*;

/// The streams a plan names, for [`Call::Effect`].
const LEAN: PanicStream = PanicStream::LeanStderr;
const PROC: PanicStream = PanicStream::ProcessStderr;

/// The payload [`Rec`]'s ways out unwind with (`resume_unwind`, which runs
/// no panic hook, so nothing is printed).
struct Ended;

struct Rec {
    settings: PanicSettings,
    abort: bool,
    calls: Vec<Call>,
}

impl PanicGlue for Rec {
    fn lean_eprintln(&mut self, line: &[u8]) {
        self.calls.push(Lean(line.to_vec()))
    }
    fn settings(&mut self) -> PanicSettings {
        self.settings
    }
    fn panic_effect(&mut self, plan: PanicPlan) {
        self.calls.push(Effect(plan.stream))
    }
    fn abort_on_panic(&mut self) -> bool {
        self.abort
    }
    fn flush_stdout(&mut self) {
        self.calls.push(Flush)
    }
    fn process_stderr(&mut self, bytes: &[u8]) {
        self.calls.push(Stderr(bytes.to_vec()))
    }
    fn internal_stderr(&mut self, line: InternalLine<'_>) {
        line.pieces(&mut |piece| self.calls.push(Internal(piece.to_vec())))
    }
    fn abort(&mut self) -> ! {
        self.calls.push(Abort);
        resume_unwind(Box::new(Ended))
    }
    fn exit(&mut self, code: i32) -> ! {
        self.calls.push(Exit(code));
        resume_unwind(Box::new(Ended))
    }
}

/// Runs `f` with a recording glue: its calls, and whether it ended the
/// process (`abort` or `exit`).
fn run(settings: PanicSettings, abort: bool, f: impl FnOnce(&mut Rec)) -> (Vec<Call>, bool) {
    let mut rec = Rec {
        settings,
        abort,
        calls: Vec::new(),
    };
    let ended = match catch_unwind(AssertUnwindSafe(|| f(&mut rec))) {
        Ok(()) => false,
        Err(e) => {
            assert!(e.is::<Ended>(), "a Rust panic, not an end");
            true
        }
    };
    (rec.calls, ended)
}

/// `LEAN_BACKTRACE=0`, nothing else: the cases' and rows' settings.
fn quiet() -> PanicSettings {
    PanicSettings::from_env(None, Some(b"0"))
}

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// A panic that goes on: its line on Lean's stream, all its bytes (a NUL
/// included, row `panic/panic.nul`), one `io_eprintln` per line, nothing
/// flushed; with backtraces on, the two more lines.
#[test]
fn report_on_lean_stream() {
    let (calls, ended) = run(quiet(), false, |g| report(b"a\0b", false, g));
    assert_eq!((calls, ended), (vec![Effect(LEAN), Lean(b("a\0b"))], false));
    let on = PanicSettings::from_env(None, None);
    let (calls, ended) = run(on, false, |g| report(b"boom", false, g));
    assert_eq!(
        (calls, ended),
        (
            vec![
                Effect(LEAN),
                Lean(b("boom")),
                Lean(b("backtrace:")),
                Lean(b("(stack trace unavailable)"))
            ],
            false
        )
    );
}

/// `LEAN_ABORT_ON_PANIC` (any value): stdout flushed, then each line and
/// its newline on the process's stderr (`std::cerr.write(line) << "\n"`),
/// then the abort; with exit-on-panic, `exit(1)` instead.
#[test]
fn report_ending() {
    let abort = PanicSettings::from_env(Some(b""), Some(b"0"));
    let (calls, ended) = run(abort, false, |g| report(b"boom", false, g));
    assert_eq!(
        (calls, ended),
        (
            vec![
                Effect(PROC),
                Flush,
                Stderr(b("boom")),
                Stderr(b("\n")),
                Abort
            ],
            true
        )
    );
    let abort_bt = PanicSettings::from_env(Some(b"1"), None);
    let (calls, _) = run(abort_bt, false, |g| report(b"boom", false, g));
    assert_eq!(
        calls,
        vec![
            Effect(PROC),
            Flush,
            Stderr(b("boom")),
            Stderr(b("\n")),
            Stderr(b("backtrace:")),
            Stderr(b("\n")),
            Stderr(b("(stack trace unavailable)")),
            Stderr(b("\n")),
            Abort
        ]
    );
    let exit = PanicSettings {
        exit_on_panic: true,
        ..quiet()
    };
    let (calls, ended) = run(exit, false, |g| report(b"boom", false, g));
    assert_eq!(
        (calls, ended),
        (
            vec![
                Effect(PROC),
                Flush,
                Stderr(b("boom")),
                Stderr(b("\n")),
                Exit(1)
            ],
            true
        )
    );
}

/// `force_stderr` (`IO.Option.getOrBlock!`): the process's stderr, stdout
/// flushed first, and the call returns.
#[test]
fn report_forced() {
    let (calls, ended) = run(quiet(), false, |g| report(b"dropped", true, g));
    assert_eq!(
        (calls, ended),
        (
            vec![Effect(PROC), Flush, Stderr(b("dropped")), Stderr(b("\n"))],
            false
        )
    );
}

/// Messages off (`lean_set_panic_messages(false)`): nothing printed and
/// nothing flushed, the end as the settings say, after an effect point when
/// there is an end.
#[test]
fn report_silent() {
    let silent = PanicSettings {
        messages: false,
        ..quiet()
    };
    assert_eq!(
        run(silent, false, |g| report(b"x", false, g)),
        (vec![], false)
    );
    let silent_abort = PanicSettings {
        abort_on_panic: true,
        ..silent
    };
    assert_eq!(
        run(silent_abort, false, |g| report(b"x", true, g)),
        (vec![Effect(PROC), Abort], true)
    );
    let silent_exit = PanicSettings {
        exit_on_panic: true,
        ..silent
    };
    assert_eq!(
        run(silent_exit, false, |g| report(b"x", false, g)),
        (vec![Effect(PROC), Exit(1)], true)
    );
}

/// `lean_internal_panic`: the line in one piece, nothing flushed, then
/// `exit(1)`, or the abort; the message up to its first NUL byte (`%s`);
/// the panic settings other than the abort flag play no part.
#[test]
fn internal_panic_steps() {
    let msg = InternalPanic::NatPowExponent.message();
    let (calls, ended) = run(quiet(), false, |g| internal_panic(msg, g));
    assert_eq!(
        (calls, ended),
        (
            vec![
                Internal(b("INTERNAL PANIC: Nat.pow exponent is too big\n")),
                Exit(1)
            ],
            true
        )
    );
    let (calls, _) = run(quiet(), true, |g| internal_panic("executed 'sorry'", g));
    assert_eq!(
        calls,
        vec![Internal(b("INTERNAL PANIC: executed 'sorry'\n")), Abort]
    );
    let (calls, _) = run(quiet(), false, |g| internal_panic("a\0b", g));
    assert_eq!(calls, vec![Internal(b("INTERNAL PANIC: a\n")), Exit(1)]);
}

/// A line longer than the stack buffer goes out whole, in pieces of 256
/// bytes.
#[test]
fn internal_panic_long_line() {
    let msg = "x".repeat(600);
    let (calls, _) = run(quiet(), false, |g| internal_panic(&msg, g));
    let pieces: Vec<&[u8]> = calls
        .iter()
        .filter_map(|c| match c {
            Internal(p) => Some(&p[..]),
            _ => None,
        })
        .collect();
    assert_eq!(
        pieces.iter().map(|p| p.len()).collect::<Vec<_>>(),
        [256, 256, 105]
    );
    assert_eq!(
        pieces.concat(),
        format!("INTERNAL PANIC: {msg}\n").into_bytes()
    );
    assert_eq!(calls.last(), Some(&Exit(1)));
}

/// Each piece ends on a character boundary (review LS3-01): with the
/// prefix's 16 bytes, `é` would straddle byte 256, so the first piece ends
/// before it; the bytes are the line's.
#[test]
fn internal_panic_pieces_on_char_boundaries() {
    let msg = format!("{}é{}", "a".repeat(239), "b".repeat(300));
    let (calls, _) = run(quiet(), false, |g| internal_panic(&msg, g));
    let pieces: Vec<&[u8]> = calls
        .iter()
        .filter_map(|c| match c {
            Internal(p) => Some(&p[..]),
            _ => None,
        })
        .collect();
    assert_eq!(
        pieces.iter().map(|p| p.len()).collect::<Vec<_>>(),
        [255, 256, 47]
    );
    assert!(pieces.iter().all(|p| std::str::from_utf8(p).is_ok()));
    assert_eq!(
        pieces.concat(),
        format!("INTERNAL PANIC: {msg}\n").into_bytes()
    );
    // a character that does not fit after a full piece's worth of others
    let msg = "é".repeat(200);
    let (calls, _) = run(quiet(), false, |g| internal_panic(&msg, g));
    let mut line = Vec::new();
    for c in &calls {
        if let Internal(p) = c {
            assert!(p.len() <= 256 && std::str::from_utf8(p).is_ok());
            line.extend_from_slice(p);
        }
    }
    assert_eq!(line, format!("INTERNAL PANIC: {msg}\n").into_bytes());
}

/// The internal panic's default writer takes the lock of the crate's
/// `stderr` model, unless this thread holds it already (review RSH3-01):
/// with this thread's guard held, the line is written without waiting.
#[test]
fn internal_line_with_stderr_held_here() {
    use super::super::handle::{stderr_for_internal_panic, stderr_held_here};
    // Another test of the parallel run may turn the cooperative locks on (a
    // global that only goes on); with them on, the line is written without
    // the lock. Read before and after the call (review RSH3-06).
    #[cfg(feature = "sched")]
    fn coop() -> bool {
        crate::sched::coop_possible()
    }
    #[cfg(not(feature = "sched"))]
    fn coop() -> bool {
        false
    }
    assert!(!stderr_held_here());
    {
        let before = coop();
        let held = stderr_for_internal_panic();
        let after = coop();
        if before != after {
            // turned on during the call: either outcome is right
            return;
        }
        if after {
            assert!(held.is_none());
        } else {
            assert!(held.is_some());
            assert!(stderr_held_here());
            // taken again on this thread: none, rather than a wait for good
            assert!(stderr_for_internal_panic().is_none());
        }
    }
    assert!(!stderr_held_here());
}

/// The uncaught error: stdout flushed, `uncaught exception: `, the text up
/// to its first NUL byte and a newline, three writes to the process's
/// stderr, then status 1, whatever the panic settings.
#[test]
fn uncaught_steps() {
    let abort = PanicSettings::from_env(Some(b"1"), None);
    let (calls, ended) = run(abort, true, |g| uncaught(b"bad\0hidden", g));
    assert_eq!(
        (calls, ended),
        (
            vec![
                Flush,
                Stderr(b("uncaught exception: ")),
                Stderr(b("bad")),
                Stderr(b("\n")),
                Exit(1)
            ],
            true
        )
    );
}

/// `IO.Process.exit`: the exit with the code, whatever the panic settings.
#[test]
fn process_exit_steps() {
    let abort = PanicSettings::from_env(Some(b"1"), None);
    assert_eq!(
        run(abort, true, |g| process_exit(3, g)),
        (vec![Exit(3)], true)
    );
    assert_eq!(
        run(quiet(), false, |g| process_exit(255, g)),
        (vec![Exit(255)], true)
    );
}

/// The glue may be a trait object.
#[test]
fn dyn_glue() {
    let (calls, _) = run(quiet(), false, |g| {
        let d: &mut dyn PanicGlue = g;
        report(b"boom", false, d)
    });
    assert_eq!(calls, vec![Effect(LEAN), Lean(b("boom"))]);
}
