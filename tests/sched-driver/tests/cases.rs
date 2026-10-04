//! Runs the Rust port (`sched-cases ID`) of every case of
//! `tests/cases/{tasks,sync,refs}` as `scripts/cases.py check` runs a
//! translator's executable, and compares its stdout, stderr and exit code
//! with the case's expected ones: native's, or the correct ones where native
//! has a Lean bug (LB-01, LB-13), or a recorded alternative of a
//! schedule-dependent case:
//! - arguments from `ID.args`, an empty environment plus `LEAN_BACKTRACE=0`
//!   and `ID.env`, a fresh working directory, stdin from `/dev/null`;
//! - stdout and stderr as pipes (merged into one with `streams = "merged"`);
//! - `expect = { hang = N }`: the output seen in N seconds, code `timeout`.
//!
//! That is how `scripts/cases.py check` runs a case and what it accepts
//! (the expected files, then the alternatives `ID.altK.*`), for the fields
//! these areas use, but for stdin (`cases.py` gives an empty pipe, which
//! reads as end of file at once, as `/dev/null` does). A case with what this
//! runner does not implement (`.stdin`, `.pipe`, `.files/`, `normalize`)
//! fails here instead of being compared differently.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The areas whose cases run through `sched`.
const AREAS: &[&str] = &["tasks", "sync", "refs"];

fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../cases")
}

/// The directory of case `id`.
fn case_dir(id: &str) -> PathBuf {
    for a in AREAS {
        let d = cases_root().join(a);
        if d.join(format!("{id}.lean")).exists() {
            return d;
        }
    }
    panic!("no case {id} in {AREAS:?}")
}

struct Outcome {
    out: Vec<u8>,
    err: Vec<u8>,
    code: String,
}

fn read(p: &Path) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

/// The `hang` and `streams` fields of a case's TOML (the only ones that
/// change how it runs).
fn meta(id: &str) -> (Option<u64>, bool) {
    let toml = std::fs::read_to_string(case_dir(id).join(format!("{id}.toml"))).unwrap_or_default();
    let mut hang = None;
    let mut merged = false;
    for line in toml.lines() {
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        if let Some(r) = l.strip_prefix("streams") {
            merged = r.contains("\"merged\"");
        }
        if let Some(r) = l.strip_prefix("expect") {
            if let Some(i) = r.find("hang") {
                let digits: String = r[i + 4..]
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(char::is_ascii_digit)
                    .collect();
                hang = digits.parse().ok();
            }
        }
    }
    (hang, merged)
}

fn run(id: &str) -> Outcome {
    let dir = case_dir(id);
    for f in [".stdin", ".pipe", ".files"] {
        assert!(
            !dir.join(format!("{id}{f}")).exists(),
            "{id}: this runner does not implement {f} (scripts/cases.py does)"
        );
    }
    let toml = std::fs::read_to_string(dir.join(format!("{id}.toml"))).unwrap_or_default();
    assert!(
        !toml
            .lines()
            .any(|l| l.trim_start().starts_with("normalize")),
        "{id}: this runner does not implement normalize (scripts/cases.py does)"
    );
    let args: Vec<String> = std::fs::read_to_string(dir.join(format!("{id}.args")))
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect();
    let env: Vec<(String, String)> = std::fs::read_to_string(dir.join(format!("{id}.env")))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let (hang, merged) = meta(id);
    run_with(id, &args, &env, hang, merged)
}

fn run_with(
    id: &str,
    args: &[String],
    env: &[(String, String)],
    hang: Option<u64>,
    merged: bool,
) -> Outcome {
    let cwd = std::env::temp_dir().join(format!("sched-driver-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sched-cases"));
    cmd.arg(id)
        .args(args)
        .env_clear()
        .env("LEAN_BACKTRACE", "0")
        .envs(env.iter().cloned())
        .current_dir(&cwd)
        .stdin(Stdio::null());
    // An AddressSanitizer run (`--features asan`) passes its options on.
    if let Ok(v) = std::env::var("ASAN_OPTIONS") {
        cmd.env("ASAN_OPTIONS", v);
    }
    let (mut out_r, err_r) = if merged {
        let (r, w) = std::io::pipe().unwrap();
        cmd.stdout(w.try_clone().unwrap()).stderr(w);
        (r, None)
    } else {
        let (r1, w1) = std::io::pipe().unwrap();
        let (r2, w2) = std::io::pipe().unwrap();
        cmd.stdout(w1).stderr(w2);
        (r1, Some(r2))
    };
    let mut child = cmd.spawn().unwrap();
    // The write ends go with `cmd`, so that the reads end with the child.
    drop(cmd);
    let t_out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out_r.read_to_end(&mut v);
        v
    });
    let t_err = err_r.map(|mut r| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = r.read_to_end(&mut v);
            v
        })
    });
    let limit = Duration::from_secs(hang.unwrap_or(60));
    let start = Instant::now();
    let code = loop {
        if let Some(st) = child.try_wait().unwrap() {
            use std::os::unix::process::ExitStatusExt;
            break match (st.code(), st.signal()) {
                (Some(c), _) => c.to_string(),
                (None, Some(s)) => (128 + s).to_string(),
                _ => "?".into(),
            };
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            break "timeout".into();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = t_out.join().unwrap();
    let err = t_err.map(|t| t.join().unwrap()).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&cwd);
    Outcome { out, err, code }
}

/// The recorded outcomes: `ID.out/.err/.code`, then `ID.altK.*`.
fn expected(id: &str) -> Vec<Outcome> {
    let dir = case_dir(id);
    let mut v = Vec::new();
    for stem in std::iter::once(id.to_string()).chain((1..10).map(|k| format!("{id}.alt{k}"))) {
        let Some(code) = read(&dir.join(format!("{stem}.code"))) else {
            continue;
        };
        v.push(Outcome {
            out: read(&dir.join(format!("{stem}.out"))).unwrap_or_default(),
            err: read(&dir.join(format!("{stem}.err"))).unwrap_or_default(),
            code: String::from_utf8_lossy(&code).trim().to_string(),
        });
    }
    v
}

fn check(id: &str) {
    let exp = expected(id);
    assert!(!exp.is_empty(), "{id}: no recorded outcome");
    let got = run(id);
    let ok = exp
        .iter()
        .any(|e| e.out == got.out && e.err == got.err && e.code == got.code);
    assert!(
        ok,
        "{id}: got code {} stdout {:?} stderr {:?}; expected code {} stdout {:?} stderr {:?}",
        got.code,
        String::from_utf8_lossy(&got.out),
        String::from_utf8_lossy(&got.err),
        exp[0].code,
        String::from_utf8_lossy(&exp[0].out),
        String::from_utf8_lossy(&exp[0].err),
    );
}

/// Every case of the areas has a port (no case is left out silently).
#[test]
fn every_case_is_ported() {
    for a in AREAS {
        for e in std::fs::read_dir(cases_root().join(a)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "lean") {
                let id = p.file_stem().unwrap().to_string_lossy().into_owned();
                assert!(
                    PORTED.contains(&id.as_str()),
                    "tests/cases/{a}/{id}.lean has no port in sched-driver"
                );
            }
        }
    }
}

/// A Rust panic (not a Lean panic: a bug of a translator or the runtime) in
/// a task on a context of its own unwinds to the context's base, and is
/// resumed on `main`'s stack, where it goes on as a panic of `main`
/// (docs/sched.md, "Rust panics"): Rust's message, status 101.
#[test]
fn a_rust_panic_in_a_context_goes_on_in_main() {
    let got = run_with("rust_panic_in_task", &[], &[], None, false);
    let err = String::from_utf8_lossy(&got.err);
    assert_eq!(got.code, "101", "stderr {err:?}");
    assert!(err.contains("a Rust panic in a task"), "stderr {err:?}");
    assert!(!err.contains("not reached"));
}

// leanrs's adversarial checks of docs/sched.md's "Why Glue::suspend is
// sound" (their proof check of sched-1 at 15b62ed, 2026-10-03), ported as
// regression tests. The programs are in `src/cases.rs`.

fn err_of(o: &Outcome) -> String {
    String::from_utf8_lossy(&o.err).into_owned()
}

/// S6: a destructor run while a context unwinds blocks there (a promise's
/// `sync` dependent waits for a sleeping task): the context suspends
/// mid-unwind, resumes, and the panic goes on in `main`.
#[test]
fn adv_block_in_drop_during_unwind() {
    let got = run_with("adv_block_in_drop_during_unwind", &[], &[], None, false);
    let err = err_of(&got);
    assert_eq!(got.code, "101", "stderr {err:?}");
    assert!(err.contains("boom in task"), "stderr {err:?}");
    assert!(
        err.contains("sync dep runs in drop during unwind"),
        "stderr {err:?}"
    );
    assert!(
        err.contains("sync dep got 7 after block, v = None"),
        "stderr {err:?}"
    );
    assert!(!err.contains("not reached"), "stderr {err:?}");
}

/// S6: a second panic in a destructor during unwinding aborts, as in plain
/// Rust.
#[test]
fn adv_panic_in_sync_dep_of_drop() {
    let got = run_with("adv_panic_in_sync_dep_of_drop", &[], &[], None, false);
    let err = err_of(&got);
    assert_eq!(got.code, "134", "stderr {err:?}");
    assert!(err.contains("second panic in sync dep"), "stderr {err:?}");
    assert!(
        err.contains("panic in a destructor during cleanup"),
        "stderr {err:?}"
    );
    assert!(!err.contains("not reached"), "stderr {err:?}");
}

/// S5: `process::exit` from a task's context runs the thread's TLS
/// destructors on the coroutine's stack; the scheduler's only forget.
#[test]
fn adv_exit_from_task() {
    let got = run_with("adv_exit_from_task", &[], &[], None, false);
    assert_eq!(got.code, "3", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "task exits\n");
    assert!(got.out.is_empty());
}

/// The ported cases, in one list: each becomes a test, and
/// `every_case_is_ported` checks the list against the cases' directories
/// (review RS1S-06 of sched-1).
macro_rules! cases {
    ($($id:ident),* $(,)?) => {
        const PORTED: &[&str] = &[$(stringify!($id)),*];
        $(
            #[test]
            fn $id() {
                check(stringify!($id));
            }
        )*
    };
}

cases!(
    checkcanceled_after_main,
    dropped_pure_task,
    exit_joins_before_flush,
    hasfinished_spin,
    runaway_io_task_unawaited,
    runaway_pure_task_referenced,
    runaway_pure_task_started,
    sleep_polling_spin,
    sync_dependent_order,
    promise_across_tasks,
    stack_overflow_in_task,
    mutex_handoff,
    condvar_turns,
    shared_mutex_readers,
    recursive_mutex,
    pure_chain_io_dep,
    pure_bind_io_dep,
    exit_from_task,
    get_in_sync_task,
    late_task_after_main,
    late_dependent_of_dedicated,
    late_wait_dedicated,
    late_wait_pool,
    late_pool_child_runs,
    late_dedicated_child_runs,
    main_waits_dedicated_child,
    wait_dep_mid_walk,
    wait_dep_mid_promise_walk,
    poll_dep_mid_walk,
    wait_any_own_dep,
    task_waits_own_dep,
    sync_dep_waits_older,
    lost_update,
);
