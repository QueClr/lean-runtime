//! The panic and exit executor (`io::panic`) in child processes, with the
//! crate's own streams (the `Native` glue), against native Lean 4.34.0's
//! recorded outcomes:
//! - the rows of `tests/cases/panic/panic.rows.toml` (each row's message,
//!   environment, stderr and status; the oracle runs with
//!   `LEAN_BACKTRACE=0`);
//! - the order of pending stdout and each path's stderr in one pipe, as the
//!   native probe of `docs/panic.md` records it (`pending;` printed first,
//!   unflushed);
//! - an internal panic allocates nothing until the process ends (a counting
//!   global allocator), and messages off, which no Lean code over `Init` and
//!   `Std` can reach, end as `object.cpp` says.
//!
//! A binary of its own without libtest (`harness = false`), whose lines
//! would mix with a child's stdout, and whose threads would allocate: each
//! check runs this binary again as a child with the case in
//! `LEAN_RUNTIME_PANIC_CHILD`; the child writes `pending;` into the crate's
//! stdout model (a pipe: fully buffered) and then takes the path.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use common::{Toml, Value};
use lean_runtime::io::panic::{self as p, Native, PanicGlue};
use lean_runtime::io::{exit, Handle};
use lean_runtime::semantics::panic::{InternalPanic, PanicSettings};

const CHILD_VAR: &str = "LEAN_RUNTIME_PANIC_CHILD";
/// The message of a child's panic, in hexadecimal (it may hold a NUL byte).
const MSG_VAR: &str = "LEAN_RUNTIME_PANIC_MSG";

// ---------------------------------------------------------------- allocations

thread_local! {
    /// Allocations of this thread are counted.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Allocations of an armed thread so far.
static COUNTED: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn note() {
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        COUNTED.fetch_add(1, Relaxed);
    }
}

// SAFETY: every call is forwarded to `System`, which upholds the contract;
// the count touches no allocation (a const thread-local and an atomic).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Native's glue, but allocations are counted from the internal panic's
/// start until its way out, which writes `ALLOCATED` on stderr if there
/// were any.
struct Watched;

impl Watched {
    fn disarm() {
        ARMED.with(|a| a.set(false));
        if COUNTED.load(Relaxed) > 0 {
            p::write_stderr_fd(b"ALLOCATED\n");
        }
    }
}

impl PanicGlue for Watched {
    fn abort(&mut self) -> ! {
        Watched::disarm();
        Native.abort()
    }
    fn exit(&mut self, code: i32) -> ! {
        Watched::disarm();
        Native.exit(code)
    }
}

/// Native's glue with messages off (`lean_set_panic_messages(false)`), and
/// exit-on-panic as `exit_on_panic` says.
struct Silent {
    exit_on_panic: bool,
}

impl PanicGlue for Silent {
    fn settings(&mut self) -> PanicSettings {
        PanicSettings {
            messages: false,
            exit_on_panic: self.exit_on_panic,
            ..p::settings()
        }
    }
}

// ---------------------------------------------------------------- the child

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// In a child: `pending;` into stdout, then the case's path; a path that
/// goes on writes `after` and exits with status 0.
fn child_case(case: &str) -> ! {
    let msg = unhex(&std::env::var(MSG_VAR).unwrap_or_default());
    let text = String::from_utf8(msg.clone()).unwrap_or_default();
    let _ = Handle::stdout().put_str(b"pending;");
    match case {
        "report" => p::report(&msg, false, &mut Native),
        "forced" => p::report(&msg, true, &mut Native),
        "silent" => p::report(
            &msg,
            false,
            &mut Silent {
                exit_on_panic: false,
            },
        ),
        "silent_exit" => p::report(
            &msg,
            false,
            &mut Silent {
                exit_on_panic: true,
            },
        ),
        "internal" => p::internal_panic(&text, &mut Native),
        "internal_watched" => {
            ARMED.with(|a| a.set(true));
            p::internal_panic(InternalPanic::OutOfMemory.message(), &mut Watched)
        }
        "uncaught" => p::uncaught(&msg, &mut Native),
        "exit" => p::process_exit(3, &mut Native),
        "force_exit" => p::process_force_exit(3, &mut Native),
        "internal_during_write" => {
            // another thread holds stderr's lock, its 200000-byte write
            // blocked on the full pipe (review RSH3-01's repro)
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let big = vec![b'a'; 200_000];
                let err = Handle::stderr();
                let mut g = err.file();
                tx.send(()).unwrap();
                let _ = g.put(&big);
            });
            rx.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            p::internal_panic("x", &mut Native)
        }
        "internal_while_holding" => {
            // this thread holds stderr's lock (an internal panic raised
            // during its own write): no wait for good, here or at the exit
            let err = Handle::stderr();
            let mut g = err.file();
            let _ = g.put(b"held;");
            p::internal_panic("x", &mut Native)
        }
        c => panic!("unknown case {c}"),
    }
    let _ = Handle::stdout().put_str(b"after\n");
    exit::exit(0)
}

/// How a child ended: its status, or 128 + the signal, as a shell says.
fn code(o: &Output) -> i32 {
    o.status
        .code()
        .unwrap_or_else(|| 128 + o.status.signal().expect("a status or a signal"))
}

/// Runs this binary again as a child with `case`, the message `msg` and the
/// environment `env` (`LEAN_ABORT_ON_PANIC` and `LEAN_BACKTRACE` unset
/// otherwise), without core dumps; with `merged`, stderr goes into stdout's
/// pipe (`2>&1`).
fn child(case: &str, msg: &[u8], env: &[(&str, &str)], merged: bool) -> Output {
    let exe = std::env::current_exe().expect("test binary path");
    let script = if merged {
        "ulimit -c 0; exec \"$0\" \"$@\" 2>&1"
    } else {
        "ulimit -c 0; exec \"$0\" \"$@\""
    };
    let mut c = Command::new("/bin/sh");
    c.args(["-c", script])
        .arg(&exe)
        .env(CHILD_VAR, case)
        .env(MSG_VAR, hex(msg))
        .env_remove("LEAN_ABORT_ON_PANIC")
        .env_remove("LEAN_BACKTRACE")
        .stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("child test process")
}

/// The child's stdout and stderr as text, and its status.
fn outcome(o: &Output) -> (String, String, i32) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
        code(o),
    )
}

// ---------------------------------------------------------------- the rows

/// A Lean string literal of a row's `args` (`"a\nb"`, `"a\x00b"`).
fn lean_string(term: &str) -> Vec<u8> {
    let inner = term
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or_else(|| panic!("not a string literal: {term}"));
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('x') => {
                let h: String = it.by_ref().take(2).collect();
                out.push(char::from(u8::from_str_radix(&h, 16).unwrap()));
            }
            e => panic!("escape {e:?} in {term}"),
        }
    }
    out.into_bytes()
}

/// The panic area's rows, and each row's environment.
fn panic_rows() -> Vec<(Value, Vec<(String, String)>)> {
    let file = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/cases/panic/panic.rows.toml"
    );
    let text = std::fs::read_to_string(file).expect("panic rows");
    Toml::rows(&text)
        .unwrap_or_else(|e| panic!("{file}: {e}"))
        .into_iter()
        .map(|row| {
            let env = row
                .get("env")
                .map(|e| {
                    e.entries()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            (row, env)
        })
        .collect()
}

/// Each row's call through the executor in a child, with the oracle's
/// `LEAN_BACKTRACE=0` and the row's environment: a `panic` row's message
/// through `report` (stderr as the row says; when it goes on, `after` and
/// status 0; when it ends, the row's status, with stdout flushed first), an
/// internal panic row's message through `internal_panic` (the row's stderr
/// and status; stdout written after the line by `exit`, lost by the
/// abort).
fn panic_rows_through_the_executor() {
    let mut seen = 0;
    for (row, env) in panic_rows() {
        let id = row.get("id").unwrap().as_str();
        let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (&k[..], &v[..])).collect();
        env.push(("LEAN_BACKTRACE", "0"));
        let ends = row.get("ends").map(|e| {
            (
                e.get("stderr").unwrap().as_str().to_string(),
                e.get("code").unwrap().as_int() as i32,
            )
        });
        let func = row.get("fn").unwrap().as_str();
        let (case, msg) = match func {
            "panic" => {
                let args = row.get("args").unwrap().strings();
                ("report", lean_string(&args[0]))
            }
            // the rule's result is `tests/rows2.rs`'s to check; here its
            // end, with Lean's message for the row
            "sorryAx" => ("internal", InternalPanic::Sorry.message().into()),
            "Array.replicate" => ("internal", InternalPanic::OutOfMemory.message().into()),
            "Nat.pow" => ("internal", InternalPanic::NatPowExponent.message().into()),
            f => panic!("{id}: no executor path for {f}"),
        };
        let o = child(case, &msg, &env, false);
        let (out, err, status) = outcome(&o);
        match (case, ends) {
            ("report", None) => {
                let want = row.get("stderr").unwrap().as_str();
                assert_eq!(
                    (&out[..], &err[..], status),
                    ("pending;after\n", want, 0),
                    "{id}"
                );
            }
            ("report", Some((want, c))) => {
                assert_eq!(
                    (&out[..], &err[..], status),
                    ("pending;", &want[..], c),
                    "{id}"
                )
            }
            (_, Some((want, c))) => {
                let out_want = if c == 1 { "pending;" } else { "" };
                assert_eq!(
                    (&out[..], &err[..], status),
                    (out_want, &want[..], c),
                    "{id}"
                )
            }
            (_, None) => panic!("{id}: an internal panic row that does not end"),
        }
        seen += 1;
    }
    assert_eq!(seen, 12, "the panic area's rows");
}

// ---------------------------------------------------------------- the order

/// Stdout and stderr in one pipe, as the native probe of `docs/panic.md`
/// records them (`Order.lean`, run with `2>&1 | cat`; the panic's message
/// is native's `Error: index out of bounds` there, `boom` here):
/// - a panic that goes on: its line on Lean's stderr, stdout not flushed
///   (it comes at the exit);
/// - under `LEAN_ABORT_ON_PANIC`: stdout flushed, the line, the abort;
/// - forced (`IO.Option.getOrBlock!`): stdout flushed, the line, and on;
/// - the internal panic: the line, then stdout at `exit(1)`; under
///   `LEAN_ABORT_ON_PANIC` stdout is lost (LB-07);
/// - the uncaught error and `IO.Process.exit`: stdout, then the line;
///   `LEAN_ABORT_ON_PANIC` plays no part;
/// - `IO.Process.forceExit` (`_Exit`; `PanicGlue::force_exit`'s default,
///   `std::process::exit`): stdout lost, as in the native probe
///   `forceexit` (`tests/io_rows.rs`); `LEAN_ABORT_ON_PANIC` plays no part.
fn order_with_pending_stdout() {
    let abort = [("LEAN_ABORT_ON_PANIC", "1"), ("LEAN_BACKTRACE", "0")];
    let quiet = [("LEAN_BACKTRACE", "0")];
    let pow = InternalPanic::NatPowExponent.message().as_bytes();
    for (case, msg, env, want, status) in [
        (
            "report",
            &b"boom"[..],
            &quiet[..],
            "boom\npending;after\n",
            0,
        ),
        ("report", b"boom", &abort, "pending;boom\n", 134),
        ("forced", b"boom", &quiet, "pending;boom\nafter\n", 0),
        (
            "internal",
            pow,
            &quiet,
            "INTERNAL PANIC: Nat.pow exponent is too big\npending;",
            1,
        ),
        (
            "internal",
            pow,
            &abort,
            "INTERNAL PANIC: Nat.pow exponent is too big\n",
            134,
        ),
        (
            "uncaught",
            b"bad",
            &quiet,
            "pending;uncaught exception: bad\n",
            1,
        ),
        (
            "uncaught",
            b"bad",
            &abort,
            "pending;uncaught exception: bad\n",
            1,
        ),
        ("exit", b"", &quiet, "pending;", 3),
        ("exit", b"", &abort, "pending;", 3),
        ("force_exit", b"", &quiet, "", 3),
        ("force_exit", b"", &abort, "", 3),
    ] {
        let o = child(case, msg, env, true);
        let (out, err, got) = outcome(&o);
        assert_eq!(
            (&out[..], &err[..], got),
            (want, "", status),
            "{case} {env:?}"
        );
    }
}

/// With backtraces on (`LEAN_BACKTRACE` unset or not `0`): `backtrace:`
/// and the frame line of a runtime without backtrace support, on the same
/// stream as the message (natively the frames, which no translator prints).
fn backtrace_lines() {
    let lines = "boom\nbacktrace:\n(stack trace unavailable)\n";
    for env in [
        &[][..],
        &[("LEAN_BACKTRACE", "")],
        &[("LEAN_BACKTRACE", "1")],
    ] {
        let o = child("report", b"boom", env, false);
        assert_eq!(
            outcome(&o),
            ("pending;after\n".into(), lines.into(), 0),
            "{env:?}"
        );
    }
    let o = child("report", b"boom", &[("LEAN_ABORT_ON_PANIC", "")], false);
    assert_eq!(outcome(&o), ("pending;".into(), lines.into(), 134));
}

/// Messages off: nothing printed and stdout not flushed; the abort loses
/// it, exit-on-panic's `exit(1)` writes it; with neither the call returns.
fn messages_off() {
    let abort = [("LEAN_ABORT_ON_PANIC", "1")];
    let o = child("silent", b"boom", &abort, false);
    assert_eq!(outcome(&o), (String::new(), String::new(), 134));
    let o = child("silent_exit", b"boom", &[], false);
    assert_eq!(outcome(&o), ("pending;".into(), String::new(), 1));
    let o = child("silent", b"boom", &[], false);
    assert_eq!(outcome(&o), ("pending;after\n".into(), String::new(), 0));
}

/// The internal panic allocates nothing from its start to its way out
/// (`LEAN_ABORT_ON_PANIC` unset, or set to the empty string, whose value
/// std copies without an allocation); its line is whole, and a NUL byte
/// ends the message (`%s`).
fn internal_panic_without_allocation() {
    let o = child("internal_watched", b"", &[], false);
    assert_eq!(
        outcome(&o),
        (
            "pending;".into(),
            "INTERNAL PANIC: out of memory\n".into(),
            1
        )
    );
    let o = child(
        "internal_watched",
        b"",
        &[("LEAN_ABORT_ON_PANIC", "")],
        false,
    );
    assert_eq!(
        outcome(&o),
        (String::new(), "INTERNAL PANIC: out of memory\n".into(), 134)
    );
    let long = "y".repeat(700);
    let o = child("internal", long.as_bytes(), &[], false);
    assert_eq!(
        outcome(&o),
        ("pending;".into(), format!("INTERNAL PANIC: {long}\n"), 1)
    );
    let o = child("internal", b"cut\0here", &[], false);
    assert_eq!(
        outcome(&o),
        ("pending;".into(), "INTERNAL PANIC: cut\n".into(), 1)
    );
}

/// Runs this binary as a child with `case` and `env`, its stdout and stderr
/// in one pipe that is read only after `delay_ms`, so that a write of more
/// than the pipe's capacity blocks meanwhile; without core dumps. The bytes
/// and the status.
fn child_slow_pipe(case: &str, env: &[(&str, &str)], delay_ms: u64) -> (Vec<u8>, i32) {
    use std::io::Read;
    let exe = std::env::current_exe().expect("test binary path");
    let (mut r, w) = std::io::pipe().expect("pipe");
    let mut c = Command::new("/bin/sh");
    c.args(["-c", "ulimit -c 0; exec \"$0\""])
        .arg(&exe)
        .env(CHILD_VAR, case)
        .env_remove(MSG_VAR)
        .env_remove("LEAN_ABORT_ON_PANIC")
        .env_remove("LEAN_BACKTRACE")
        .stdin(Stdio::null())
        .stdout(w.try_clone().expect("pipe"))
        .stderr(w);
    for (k, v) in env {
        c.env(k, v);
    }
    let mut proc = c.spawn().expect("child");
    drop(c);
    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    let mut out = Vec::new();
    r.read_to_end(&mut out).expect("read");
    let st = proc.wait().expect("wait");
    let code = st.code().unwrap_or_else(|| 128 + st.signal().unwrap());
    (out, code)
}

/// Review RSH3-01: an internal panic while another thread's write to the
/// process's stderr is in progress waits for it, as native's `fprintf`
/// waits for the `FILE` lock (native: the line at offset 200000, every byte
/// of the other write before it, also under `LEAN_ABORT_ON_PANIC`; the
/// crate wrote it inside the other write without the lock, and lost 3392
/// bytes with the abort). And with this thread holding the lock, the line
/// is written at once and the exit skips stderr, where a relock would wait
/// for good.
fn internal_panic_waits_for_stderr() {
    let line = "INTERNAL PANIC: x\n";
    let other = "a".repeat(200_000);
    let (out, code) = child_slow_pipe("internal_during_write", &[], 500);
    let out = String::from_utf8(out).unwrap();
    assert_eq!((out.find("INTERNAL"), code), (Some(200_000), 1));
    assert_eq!(out, format!("{other}{line}pending;"));
    let abort = [("LEAN_ABORT_ON_PANIC", "1")];
    let (out, code) = child_slow_pipe("internal_during_write", &abort, 500);
    let out = String::from_utf8(out).unwrap();
    assert_eq!((out.find("INTERNAL"), code), (Some(200_000), 134));
    assert_eq!(out, format!("{other}{line}"));
    for (env, status, tail) in [(&[][..], 1, "pending;"), (&abort[..], 134, "")] {
        let (out, code) = child_slow_pipe("internal_while_holding", env, 0);
        assert_eq!(
            (String::from_utf8(out).unwrap(), code),
            (format!("held;{line}{tail}"), status),
            "{env:?}"
        );
    }
}

fn main() {
    if let Ok(case) = std::env::var(CHILD_VAR) {
        child_case(&case);
    }
    if cfg!(miri) {
        return;
    }
    for (name, check) in [
        (
            "panic rows through the executor",
            panic_rows_through_the_executor as fn(),
        ),
        ("order with pending stdout", order_with_pending_stdout),
        ("backtrace lines", backtrace_lines),
        ("messages off", messages_off),
        (
            "internal panic without allocation",
            internal_panic_without_allocation,
        ),
        (
            "internal panic waits for stderr",
            internal_panic_waits_for_stderr,
        ),
    ] {
        check();
        println!("io_panic: {name}: ok");
    }
}
