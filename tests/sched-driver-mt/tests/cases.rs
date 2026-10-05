//! Runs the Rust port (`sched-cases-mt ID`) of every case of
//! `tests/cases/{tasks,sync,refs,taskio}` in threads mode, 5 times each, as
//! `scripts/cases.py check` runs a translator's executable (the
//! single-thread driver's `runner`), and requires every run to give the
//! case's expected outcome (native's, or the correct one where native has a
//! Lean bug: then never native's) or a recorded alternative
//! (docs/threads.md, section 4).
//!
//! Threads mode is not deterministic, and neither is native: a case records
//! every outcome native showed (`ID.altK.*` of a `schedule_dependent` case),
//! and its events are tens of milliseconds apart. Two kinds of alternative
//! are not accepted here:
//! - none of a case whose `deviations` give `lean_runtime` a known
//!   difference of the single-thread scheduler (`LSCHED-xx`, docs/sched.md):
//!   its alternative is the deferred model's outcome, written by hand, and
//!   threads mode, native's model, must give native's;
//! - nothing a run of this driver showed: an alternative comes from native
//!   runs only (`scripts/cases.py expect`).
//!
//! The binary lists what it runs (`--list`) and the cases whose ports run
//! over the single-thread scheduler only, with the reason
//! (`--single-thread-only`, `cases::SINGLE_THREAD_ONLY`); every case of the
//! areas is in one of the two.
//!
//! The cases run `SCHED_MT_JOBS` at a time (default 6; the host is shared),
//! each case's runs one after the other. `SCHED_MT_CASES=ID,ID,...` runs
//! only those.

#[path = "../../sched-driver/tests/runner/mod.rs"]
mod runner;
use runner::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// The driver's binary.
const EXE: &str = env!("CARGO_BIN_EXE_sched-cases-mt");

/// The areas whose cases run in threads mode.
const AREAS: &[&str] = &["tasks", "sync", "refs", "taskio"];

/// No case of another area runs here.
const WITH_TASKS: &[(&str, &str)] = &[];

/// The runs of each case.
const RUNS: usize = 5;

/// The lines the binary prints for `flag`.
fn listed(flag: &str) -> Vec<String> {
    let out = std::process::Command::new(EXE)
        .arg(flag)
        .output()
        .expect("sched-cases-mt");
    assert!(out.status.success(), "sched-cases-mt {flag}: {out:?}");
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .lines()
        .map(String::from)
        .collect()
}

/// A TOML string's text (`"..."`), without its quotes.
fn unquote(v: &str) -> &str {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(v)
}

/// The entry `key` of an inline table (`{ k = "v", ... }`), by a split on
/// the commas outside quotes.
fn inline_entry<'a>(table: &'a str, key: &str) -> Option<&'a str> {
    let inner = table.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut entries = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in inner.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                entries.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    entries.push(&inner[start..]);
    entries.into_iter().find_map(|e| {
        let (k, v) = e.split_once('=')?;
        (unquote(k) == key).then(|| unquote(v))
    })
}

/// The value of the case's `deviations` entry for `lean_runtime`, in any of
/// TOML's forms: `deviations = { lean_runtime = "..." }`,
/// `deviations.lean_runtime = "..."`, or a `[deviations]` table.
fn lean_runtime_deviation(toml: &str) -> Option<String> {
    let mut table = String::new();
    for line in toml.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') {
            table = l.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let found = match (table.as_str(), unquote(k)) {
            ("", "deviations") => inline_entry(v, "lean_runtime"),
            ("", "deviations.lean_runtime") | ("deviations", "lean_runtime") => Some(unquote(v)),
            _ => None,
        };
        if let Some(d) = found {
            return Some(d.to_string());
        }
    }
    None
}

/// Whether the case's `deviations` give `lean_runtime` a known difference
/// of the single-thread scheduler (`LSCHED-xx`): its alternatives are that
/// scheduler's outcome, not native's. A TOML that names `LSCHED-` or
/// `lean_runtime` outside a comment, but where this reader finds no such
/// deviation, fails the test (review RT3-02): a form it misses would let
/// the single-thread scheduler's alternatives back in.
fn single_thread_difference(id: &str) -> bool {
    let toml = std::fs::read_to_string(case_dir(id).join(format!("{id}.toml"))).unwrap_or_default();
    let dev = lean_runtime_deviation(&toml);
    let lsched = dev.as_deref().is_some_and(|d| d.contains("LSCHED-"));
    let named = |s: &str| {
        toml.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .any(|l| l.contains(s))
    };
    assert!(
        lsched || !(named("LSCHED-") || (named("lean_runtime") && dev.is_none())),
        "{id}.toml names LSCHED- or lean_runtime, but no LSCHED-xx deviation of lean_runtime \
         is found (its lean_runtime deviation: {dev:?}): a form this test does not read"
    );
    lsched
}

/// The outcomes threads mode accepts for case `id`: `ID.out/.err/.code`
/// first, then the alternatives, but none of an LSCHED case.
fn accepted(id: &str) -> Vec<Outcome> {
    assert!(
        case_dir(id).join(format!("{id}.code")).exists(),
        "{id}: no {id}.code, so its expected outcome is missing"
    );
    let mut exp = expected(id);
    if single_thread_difference(id) {
        exp.truncate(1);
    }
    exp
}

fn show(o: &Outcome) -> String {
    format!(
        "code {} stdout {:?} stderr {:?}",
        o.code,
        String::from_utf8_lossy(&o.out),
        String::from_utf8_lossy(&o.err)
    )
}

/// `RUNS` runs of case `id`, one after the other: an error names the first
/// run whose outcome is not accepted.
fn check_runs(id: &str) -> Result<(), String> {
    let acc = accepted(id);
    for k in 1..=RUNS {
        let got = run(id);
        if !acc
            .iter()
            .any(|e| e.out == got.out && e.err == got.err && e.code == got.code)
        {
            return Err(format!(
                "{id}: run {k} of {RUNS}: got {}; expected {}{}",
                show(&got),
                show(&acc[0]),
                if acc.len() > 1 {
                    format!(" or one of {} alternatives", acc.len() - 1)
                } else {
                    String::new()
                }
            ));
        }
    }
    Ok(())
}

/// Every case of the areas runs here, or is single-thread-only with a
/// reason; nothing else is listed.
#[test]
fn every_case_runs_or_is_single_thread_only() {
    let runs = listed("--list");
    let only: Vec<(String, String)> = listed("--single-thread-only")
        .into_iter()
        .map(|l| {
            let (id, why) = l.split_once('\t').expect("ID\\tREASON");
            (id.to_string(), why.to_string())
        })
        .collect();
    for (id, why) in &only {
        assert!(
            !why.trim().is_empty(),
            "{id}: single-thread-only without a reason"
        );
        assert!(!runs.contains(id), "{id}: both run and single-thread-only");
    }
    let mut cases = Vec::new();
    for a in AREAS {
        for e in std::fs::read_dir(cases_root().join(a)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "lean") {
                cases.push(p.file_stem().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    for id in &cases {
        assert!(
            runs.contains(id) || only.iter().any(|(o, _)| o == id),
            "tests/cases/{id}: neither run in threads mode nor single-thread-only"
        );
    }
    for id in runs.iter().chain(only.iter().map(|(o, _)| o)) {
        assert!(cases.contains(id), "{id}: not a case of {AREAS:?}");
    }
    // every case's TOML in a form the LSCHED check reads (RT3-02), and the
    // four known differences found
    let lsched: Vec<&String> = runs
        .iter()
        .filter(|id| single_thread_difference(id))
        .collect();
    for id in [
        "runaway_pure_task_before_io",
        "runaway_pure_passed_over",
        "runaway_pure_before_awaited",
        "picked_task_sleeping_worker",
    ] {
        assert!(
            lsched.iter().any(|l| *l == id),
            "{id}: its LSCHED deviation is not detected"
        );
    }
}

/// The LSCHED reader finds a `lean_runtime` deviation in each of TOML's
/// forms (review RT3-02).
#[test]
fn lean_runtime_deviation_forms() {
    let forms = [
        "deviations = { lean_runtime = \"LSCHED-01\", leanrs = \"DV26 (b)\" }",
        "deviations = {leanrs = \"a, b\",\"lean_runtime\"=\"LSCHED-02\"}",
        "deviations.lean_runtime = \"LSCHED-03\"",
        "id = \"x\"\n[deviations]\nleanrs = \"DV2\"\nlean_runtime = \"LSCHED-04\"",
    ];
    for (k, f) in forms.iter().enumerate() {
        assert_eq!(
            lean_runtime_deviation(f).as_deref(),
            Some(format!("LSCHED-0{}", k + 1).as_str()),
            "{f}"
        );
    }
    assert_eq!(
        lean_runtime_deviation("# deviations = { lean_runtime = \"LSCHED-01\" }"),
        None
    );
    assert_eq!(
        lean_runtime_deviation("deviations = { leanrs = \"DV2\" }"),
        None
    );
}

/// Every listed case, `RUNS` times.
#[test]
fn cases_in_threads_mode() {
    let mut ids = listed("--list");
    if let Ok(only) = std::env::var("SCHED_MT_CASES") {
        let want: Vec<&str> = only.split(',').map(str::trim).collect();
        for w in &want {
            assert!(ids.iter().any(|i| i == w), "SCHED_MT_CASES: no case {w}");
        }
        ids.retain(|i| want.contains(&i.as_str()));
    }
    // the longest first: the cases that run until their `hang` bound
    ids.sort_by_key(|id| std::cmp::Reverse(meta(id).0.unwrap_or(0)));
    let jobs = std::env::var("SCHED_MT_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6usize)
        .max(1);
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(id) = ids.get(i) else { break };
                match check_runs(id) {
                    Ok(()) => println!("ok {id} ({RUNS} runs)"),
                    Err(e) => failures.lock().unwrap().push(e),
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} cases differ in threads mode:\n{}",
        failures.len(),
        ids.len(),
        failures.join("\n")
    );
}
