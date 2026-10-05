//! How both drivers' tests run a case's port: as `scripts/cases.py check`
//! runs a translator's executable, then compared with the case's expected
//! stdout, stderr and exit code (native's, or the correct ones where native
//! has a Lean bug), or a recorded alternative:
//! - arguments from `ID.args`, an empty environment plus `LEAN_BACKTRACE=0`
//!   and `ID.env`, a fresh working directory, stdin from `/dev/null`;
//! - stdout and stderr as pipes (merged into one with `streams = "merged"`);
//! - `expect = { hang = N }`: the output seen in N seconds, code `timeout`;
//! - `ID.pipe`: the bash line run with `pipefail` instead of the executable,
//!   with `$BIN` (the executable and the case's id), `$ARGS` and
//!   `PATH=/usr/bin:/bin`.
//!
//! That is how `scripts/cases.py check` runs a case and what it accepts
//! (the expected files, then the alternatives `ID.altK.*`), for the fields
//! these areas use, but for stdin (`cases.py` gives an empty pipe, which
//! reads as end of file at once, as `/dev/null` does). A case with what this
//! runner does not implement (`.stdin`, `.files/`, `normalize`) fails here
//! instead of being compared differently.
//!
//! The test that includes this module defines `EXE` (the driver's binary),
//! `AREAS` (the areas whose cases it runs) and `WITH_TASKS` (cases of other
//! areas, with their areas). `tests/sched-driver/tests/cases.rs` and
//! `tests/sched-driver-mt/tests/cases.rs` (by a `#[path]` include) use it.

// Each driver's test uses a part of it.
#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../cases")
}

/// The directory of case `id`, if it is a case (the driver's own programs,
/// `rust_panic_in_task` and the `adv_*` checks, are not).
pub(crate) fn find_case_dir(id: &str) -> Option<PathBuf> {
    if let Some((area, _)) = super::WITH_TASKS.iter().find(|(_, c)| *c == id) {
        return Some(cases_root().join(area));
    }
    super::AREAS
        .iter()
        .map(|a| cases_root().join(a))
        .find(|d| d.join(format!("{id}.lean")).exists())
}

/// The directory of case `id`.
pub(crate) fn case_dir(id: &str) -> PathBuf {
    find_case_dir(id).unwrap_or_else(|| panic!("no case {id} in {:?}", super::AREAS))
}

pub(crate) struct Outcome {
    pub(crate) out: Vec<u8>,
    pub(crate) err: Vec<u8>,
    pub(crate) code: String,
}

pub(crate) fn read(p: &Path) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

/// The `hang` and `streams` fields of a case's TOML (the only ones that
/// change how it runs).
pub(crate) fn meta(id: &str) -> (Option<u64>, bool) {
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

pub(crate) fn run(id: &str) -> Outcome {
    let dir = case_dir(id);
    for f in [".stdin", ".files"] {
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

pub(crate) fn run_with(
    id: &str,
    args: &[String],
    env: &[(String, String)],
    hang: Option<u64>,
    merged: bool,
) -> Outcome {
    // `ID.pipe`: a bash line run with pipefail instead of the executable,
    // with `$BIN` (here the executable and the case's id) and `$ARGS`, as
    // `scripts/cases.py` runs it.
    let pipe =
        find_case_dir(id).and_then(|d| std::fs::read_to_string(d.join(format!("{id}.pipe"))).ok());
    run_full(id, args, env, hang, merged, pipe)
}

/// The driver's program `id` run by the bash line `line` (with `$BIN`), as a
/// case's `.pipe`.
pub(crate) fn run_line(id: &str, line: &str, hang: Option<u64>) -> Outcome {
    run_full(id, &[], &[], hang, false, Some(line.to_owned()))
}

pub(crate) fn run_full(
    id: &str,
    args: &[String],
    env: &[(String, String)],
    hang: Option<u64>,
    merged: bool,
    pipe: Option<String>,
) -> Outcome {
    let cwd = std::env::temp_dir().join(format!("sched-driver-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let exe = super::EXE;
    let mut cmd = match &pipe {
        Some(line) => {
            let mut c = Command::new("/bin/bash");
            c.args(["-o", "pipefail", "-c", line.trim()]);
            c
        }
        None => {
            let mut c = Command::new(exe);
            c.arg(id).args(args);
            c
        }
    };
    cmd.env_clear()
        .env("LEAN_BACKTRACE", "0")
        .envs(env.iter().cloned())
        .current_dir(&cwd)
        .stdin(Stdio::null());
    if pipe.is_some() {
        cmd.env("BIN", format!("{exe} {id}"))
            .env("ARGS", args.join(" "))
            .env("PATH", "/usr/bin:/bin");
    }
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
    // In a process group of its own, which a timeout kills whole (the
    // twin behind a `.pipe` line, and its children), as `scripts/cases.py`
    // does.
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
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
            // the group, by its id (the child's pid), never by name
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
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
pub(crate) fn expected(id: &str) -> Vec<Outcome> {
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
