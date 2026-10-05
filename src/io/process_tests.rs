//! Tests of `process.rs`, ported from leanrs's `rt/leanrs_rt/tests/io.rs`
//! (`io_process_spawn`, `io_child_*`, `io_process_output`,
//! `io_spawn_cwd_failures`, `io_process_spawn_script`,
//! `io_failed_child_stdin`). Expected values are native Lean's (leanrs probes
//! `validate/io/proc_spawn`, `proc_output`, `proc_inherit` and A755 on
//! v4.34.0-rc1, rechecked on 4.34.0 by `tests/cases/process`). Each test
//! kills only the children it spawned. Miri cannot spawn processes.
#![cfg(not(miri))]

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;

const MISSING: &[u8] = b"lean-runtime-no-such-program-xyz";

fn cfg(stdin: Stdio, stdout: Stdio, stderr: Stdio) -> StdioConfig {
    StdioConfig {
        stdin,
        stdout,
        stderr,
    }
}

fn args<'a>(cmd: &'a [u8], argv: &'a [&'a [u8]]) -> SpawnArgs<'a> {
    SpawnArgs {
        cmd,
        args: argv,
        cwd: None,
        env: &[],
        inherit_env: true,
        setsid: false,
    }
}

fn read_all(h: &Handle) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = h.read(&mut buf).unwrap();
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

fn no_such_process(code: u32) -> IoError {
    IoError::NoSuchThing(None, code, "no such process".to_owned())
}

fn broken_pipe() -> IoError {
    IoError::ResourceVanished(32, "broken pipe".to_owned())
}

/// `output`'s result as (code, stdout, stderr).
fn run(a: &SpawnArgs, input: Option<&[u8]>) -> Result<(u32, String, String), IoError> {
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let code = output(a, input, &mut o, &mut e)?;
    Ok((
        code,
        String::from_utf8(o).unwrap(),
        String::from_utf8(e).unwrap(),
    ))
}

/// `run` of a script this test has just written: while another test thread
/// spawns a child, that child holds a copy of the script's write descriptor
/// until its `exec` (close-on-exec), and `execve` of the script fails with
/// `ETXTBSY` meanwhile (the child's "could not execute" failure), so a
/// failure to start is retried.
fn run_fresh_script(a: &SpawnArgs) -> (u32, String, String) {
    for _ in 0..100 {
        let r = run(a, None).unwrap();
        if r.0 != 255 || !r.2.starts_with("could not execute") {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    run(a, None).unwrap()
}

fn missing_message() -> String {
    format!(
        "could not execute external process '{}'\n",
        std::str::from_utf8(MISSING).unwrap()
    )
}

/// lean (proc_spawn): `cat` with both pipes, standard input closed through
/// `takeStdin`; `sh` with a nonzero exit; a missing program is the child's
/// failure, status 255, its message on the child's standard error; the
/// arguments are C strings.
#[test]
fn io_process_spawn() {
    let c = cfg(Stdio::Piped, Stdio::Piped, Stdio::Null);
    let child = spawn(c, &args(b"cat", &[])).unwrap();
    let stdin = child.stdin.unwrap();
    stdin.put_str(b"through cat\nsecond line\n").unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    assert_eq!(
        read_all(child.stdout.as_ref().unwrap()),
        b"through cat\nsecond line\n"
    );
    assert_eq!(child.process.wait().unwrap(), 0);

    let c = cfg(Stdio::Null, Stdio::Piped, Stdio::Piped);
    let child = spawn(c, &args(b"sh", &[b"-c", b"echo e1 >&2; echo o1; exit 7"])).unwrap();
    assert_eq!(read_all(child.stderr.as_ref().unwrap()), b"e1\n");
    assert_eq!(read_all(child.stdout.as_ref().unwrap()), b"o1\n");
    assert_eq!(child.process.wait().unwrap(), 7);

    let child = spawn(c, &args(MISSING, &[])).unwrap();
    assert_eq!(
        read_all(child.stderr.as_ref().unwrap()),
        missing_message().as_bytes()
    );
    assert_eq!(child.process.wait().unwrap(), 255);

    let child = spawn(c, &args(b"echo", &[b"a\0b", b"c"])).unwrap();
    assert_eq!(read_all(child.stdout.as_ref().unwrap()), b"a c\n");
    assert_eq!(child.process.wait().unwrap(), 0);
}

/// lean (proc_spawn): a second `wait` and a `tryWait` after it give
/// `no such thing (error code: 10, no such process)`.
#[test]
fn io_child_wait() {
    let c = cfg(Stdio::Null, Stdio::Null, Stdio::Null);
    let child = spawn(c, &args(b"sh", &[b"-c", b"exit 3"])).unwrap();
    assert_eq!(child.process.wait().unwrap(), 3);
    assert_eq!(child.process.wait().unwrap_err(), no_such_process(10));
    assert_eq!(child.process.try_wait().unwrap_err(), no_such_process(10));
}

/// lean (proc_spawn): `true` polled with `tryWait` until it ends gives
/// `some 0`.
#[test]
fn io_child_try_wait() {
    let c = cfg(Stdio::Null, Stdio::Null, Stdio::Null);
    let child = spawn(c, &args(b"true", &[])).unwrap();
    let code = loop {
        match child.process.try_wait().unwrap() {
            Some(code) => break code,
            None => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    };
    assert_eq!(code, 0);
    assert_eq!(child.process.try_wait().unwrap_err(), no_such_process(10));
}

/// lean (proc_spawn): `sleep 30` killed gives 137; a `kill` after the wait
/// gives `no such thing (error code: 3, no such process)`.
#[test]
fn io_child_kill() {
    let c = cfg(Stdio::Null, Stdio::Null, Stdio::Null);
    let child = spawn(c, &args(b"sleep", &[b"30"])).unwrap();
    assert_eq!(child.process.try_wait().unwrap(), None);
    child.process.kill().unwrap();
    assert_eq!(child.process.wait().unwrap(), 137);
    assert_eq!(child.process.kill().unwrap_err(), no_such_process(3));
}

/// lean (proc_spawn): `pid positive: true`; a child that could not start has
/// pid 0 (leanrs DV15 (a); natively it has a pid, case `failed_child_pid`).
#[test]
fn io_child_pid() {
    let c = cfg(Stdio::Null, Stdio::Null, Stdio::Null);
    let child = spawn(c, &args(b"true", &[])).unwrap();
    assert!(child.process.pid() > 0);
    child.process.wait().unwrap();
    // a child that cannot start is a real process too (its stand-in)
    let failed = spawn(c, &args(MISSING, &[])).unwrap();
    let pid = failed.process.pid();
    assert!(pid > 0 && pid < 1 << 22, "{pid}");
    assert_eq!(failed.process.wait().unwrap(), 255);
}

/// `takeStdin` keeps the process and its `setsid` flag (LB-14; native case
/// `take_stdin_setsid` drops it); the fields that are not piped are `()`.
#[test]
fn io_child_take_stdin() {
    let c = cfg(Stdio::Piped, Stdio::Null, Stdio::Null);
    let mut a = args(b"cat", &[]);
    a.setsid = true;
    let child = spawn(c, &a).unwrap();
    assert!(child.stdout.is_none() && child.stderr.is_none());
    assert!(child.process.setsid());
    let p = child.process.take_stdin();
    assert!(p.setsid() && p.pid() == child.process.pid());
    drop(child.stdin);
    assert_eq!(p.wait().unwrap(), 0);
    assert_eq!(child.process.wait().unwrap_err(), no_such_process(10));
}

/// LB-14: `kill` after `takeStdin` reaches a `setsid` child's process group,
/// so its background grandchild dies too (natively it survives).
#[test]
fn take_stdin_kill_reaches_group() {
    let c = cfg(Stdio::Piped, Stdio::Piped, Stdio::Null);
    let mut a = args(b"sh", &[b"-c", b"sleep 30 & echo $!; cat >/dev/null; wait"]);
    a.setsid = true;
    let child = spawn(c, &a).unwrap();
    let mut line = Vec::new();
    child.stdout.as_ref().unwrap().get_line(&mut line).unwrap();
    let gc: i32 = String::from_utf8(line).unwrap().trim().parse().unwrap();
    let p = child.process.take_stdin();
    drop(child.stdin);
    p.kill().unwrap();
    assert_eq!(p.wait().unwrap(), 137);
    let gone = (0..200).any(|_| {
        let st = std::fs::read_to_string(format!("/proc/{gc}/stat")).unwrap_or_default();
        let z = st.is_empty() || st[st.rfind(')').map_or(0, |i| i + 2)..].starts_with('Z');
        if !z {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        z
    });
    assert!(gone, "grandchild {gc} survived");
}

/// lean (proc_output): `echo`, `cat` with input, a nonzero exit, a signal, a
/// missing program, standard output that is not UTF-8.
#[test]
fn io_process_output() {
    assert_eq!(
        run(&args(b"echo", &[b"hello", b"pipe world"]), None).unwrap(),
        (0, "hello pipe world\n".to_owned(), String::new())
    );
    assert_eq!(
        run(&args(b"cat", &[]), Some(b"line one\nline two\n"))
            .unwrap()
            .1,
        "line one\nline two\n"
    );
    assert_eq!(
        run(
            &args(b"sh", &[b"-c", b"echo to-out; echo to-err >&2; exit 3"]),
            None
        )
        .unwrap(),
        (3, "to-out\n".to_owned(), "to-err\n".to_owned())
    );
    assert_eq!(
        run(&args(b"sh", &[b"-c", b"kill -9 $$"]), None).unwrap().0,
        137
    );
    assert_eq!(
        run(&args(MISSING, &[b"a"]), None).unwrap(),
        (255, String::new(), missing_message())
    );
    assert_eq!(
        run(&args(b"printf", &[b"\\377\\376"]), None).unwrap_err(),
        IoError::UserError("Tried to read from handle containing non UTF-8 data.".to_owned())
    );
    // both pipes are read at once: 300000 bytes on stdout after 200000 on stderr
    let (code, o, e) = run(
        &args(
            b"sh",
            &[b"-c", b"yes b 2>/dev/null | head -c 200000 >&2; yes a 2>/dev/null | head -c 300000; exit 4"],
        ),
        None,
    )
    .unwrap();
    assert_eq!((code, o.len(), e.len()), (4, 300000, 200000));
}

/// A sink that stops once it would hold more than `cap` bytes, as a glue's
/// sink whose reservation failed (leanrs's `Bytes`).
struct Capped {
    v: Vec<u8>,
    cap: usize,
    stopped: bool,
}

impl ByteSink for Capped {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if self.stopped {
            return;
        }
        if self.v.len() + bytes.len() > self.cap {
            self.stopped = true;
            self.v = Vec::new();
            return;
        }
        self.v.extend_from_slice(bytes);
    }

    fn stopped(&self) -> bool {
        self.stopped
    }
}

/// AR-5: once a sink stops, `output` returns at once with `ENOMEM`'s error,
/// whether the child writes standard output or standard error without end
/// and keeps the other pipe open (`yes` alone: it never reaches end of file,
/// and before the fix `output` read and dropped its bytes forever). The
/// other sink is untouched. The call runs on a thread so that a regression
/// fails the test instead of hanging it.
#[test]
fn output_stops_with_its_sink() {
    for (script, stdout_stops) in [(&b"exec yes"[..], true), (&b"exec yes >&2"[..], false)] {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mk = || Capped {
                v: Vec::new(),
                cap: 1 << 20,
                stopped: false,
            };
            let (mut o, mut e) = (mk(), mk());
            let r = output(&args(b"sh", &[b"-c", script]), None, &mut o, &mut e);
            let _ = tx.send((r, o.stopped, e.stopped, o.v.len(), e.v.len()));
        });
        let (r, os, es, ol, el) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("output returns once its sink has stopped");
        assert_eq!(
            r.unwrap_err(),
            IoError::ResourceExhausted(None, 12, "not enough memory".to_owned())
        );
        assert_eq!((os, es), (stdout_stops, !stdout_stops));
        assert_eq!((ol, el), (0, 0));
    }
}

/// A UTF-8 character split across reads is valid; a bad byte or a cut
/// character at the end is not.
#[test]
fn utf8_stream() {
    let text = "a€😀é".as_bytes();
    for cut in 0..=text.len() {
        let mut s = Utf8Stream::default();
        s.feed(&text[..cut]);
        s.feed(&text[cut..]);
        assert!(s.valid(), "cut at {cut}");
    }
    for (pieces, ok) in [
        (&[&b"\xe2\x82"[..]][..], false),
        (&[&b"\xe2"[..], &b"\x82"[..], &b"\xac"[..]][..], true),
        (&[&b"\xe2"[..], &b"x"[..]][..], false),
        (&[&b"\xff"[..]][..], false),
        (&[&b"\xed\xa0\x80"[..]][..], false),
    ] {
        let mut s = Utf8Stream::default();
        for p in pieces {
            s.feed(p);
        }
        assert_eq!(s.valid(), ok, "{pieces:?}");
    }
}

/// Runs this test binary again as a child running only `test`, with
/// `LEAN_RUNTIME_TEST_CHILD` set, and returns its output.
fn run_self(test: &str) -> std::process::Output {
    run_self_as(test, "1")
}

/// The same with `LEAN_RUNTIME_TEST_CHILD=case`.
fn run_self_as(test: &str, case: &str) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env("LEAN_RUNTIME_TEST_CHILD", case)
        .output()
        .unwrap()
}

fn pwd_in(d: &str) -> Result<(u32, String, String), IoError> {
    run(
        &SpawnArgs {
            cwd: Some(d.as_bytes()),
            ..args(b"pwd", &[])
        },
        None,
    )
}

fn cd_failure(d: &str) -> (u32, String, String) {
    (
        255,
        String::new(),
        format!("could not change directory to {d}\n"),
    )
}

/// The `cwd` cases of lean (proc_inherit), in `base`: a directory without
/// search permission, a regular file, a path through one, the empty path and
/// one cut at a NUL byte are the directory's failure, whatever the program;
/// search permission alone enters a directory; a relative `cwd` starts from
/// the process's working directory. The process's working directory never
/// changes.
fn cwd_cases(base: &std::path::Path) {
    let here = std::env::current_dir().unwrap();
    let locked = base.join("locked");
    let search_only = base.join("searchonly");
    let plain = base.join("plain");
    std::fs::create_dir(&locked).unwrap();
    std::fs::create_dir(&search_only).unwrap();
    std::fs::write(&plain, "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&search_only, std::fs::Permissions::from_mode(0o100)).unwrap();
    let l = locked.to_str().unwrap();
    if !nix::unistd::Uid::effective().is_root() {
        assert_eq!(pwd_in(l).unwrap(), cd_failure(l));
        let missing = SpawnArgs {
            cwd: Some(l.as_bytes()),
            ..args(MISSING, &[])
        };
        assert_eq!(run(&missing, None).unwrap(), cd_failure(l));
    }
    let so = SpawnArgs {
        cwd: Some(search_only.to_str().unwrap().as_bytes()),
        ..args(b"sh", &[b"-c", b"basename \"$PWD\""])
    };
    assert_eq!(
        run(&so, None).unwrap(),
        (0, "searchonly\n".to_owned(), String::new())
    );
    let p = plain.to_str().unwrap();
    assert_eq!(pwd_in(p).unwrap(), cd_failure(p));
    let through = format!("{p}/sub");
    assert_eq!(pwd_in(&through).unwrap(), cd_failure(&through));
    assert_eq!(pwd_in("").unwrap(), cd_failure(""));
    // the cwd is a C string
    let nul = format!("{p}\0junk");
    assert_eq!(pwd_in(&nul).unwrap(), cd_failure(p));
    // relative to the process's working directory, which may change
    std::fs::create_dir_all(base.join("rel/sub")).unwrap();
    let rel = base.join("rel");
    std::env::set_current_dir(&rel).unwrap();
    let got = pwd_in("sub").unwrap();
    std::env::set_current_dir(&here).unwrap();
    assert_eq!(got.1, format!("{}/sub\n", rel.display()));
    assert_eq!(std::env::current_dir().unwrap(), here);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&search_only, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// A process that may not search its own working directory: a relative
/// `cwd` fails as the forked child's `chdir` does; an absolute one runs
/// (natively), or, in the fallback, is the spawn's `EACCES` (it could not come
/// back); a spawn without `cwd` runs. The working directory never changes.
fn nosearch_cases(base: &std::path::Path, fallback: bool) {
    let back = std::env::current_dir().unwrap();
    let here = base.join("nosearch");
    std::fs::create_dir_all(here.join("sub")).unwrap();
    std::env::set_current_dir(&here).unwrap();
    std::fs::set_permissions(&here, std::fs::Permissions::from_mode(0o600)).unwrap();
    let rel = pwd_in("sub");
    let abs = pwd_in("/");
    let none = run(&args(b"sh", &[b"-c", b"echo ran"]), None);
    std::fs::set_permissions(&here, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::env::current_dir().unwrap(), here);
    std::env::set_current_dir(&back).unwrap();
    assert_eq!(rel.unwrap(), cd_failure("sub"));
    if fallback {
        assert_eq!(
            abs.unwrap_err(),
            IoError::PermissionDenied(None, 13, "permission denied".to_owned())
        );
    } else {
        assert_eq!(abs.unwrap(), (0, "/\n".to_owned(), String::new()));
    }
    assert_eq!(none.unwrap(), (0, "ran\n".to_owned(), String::new()));
}

/// Whether a thread may unshare its file-system attributes here (not under
/// a seccomp profile that refuses `unshare`).
fn unshare_allowed() -> bool {
    std::thread::spawn(|| nix::sched::unshare(nix::sched::CloneFlags::CLONE_FS).is_ok())
        .join()
        .unwrap()
}

/// lean (proc_inherit, `process/cwd_nosearch`): the `cwd` cases on the
/// spawner thread, then, each in a child process (they change the working
/// directory), from a working directory the process may not search, and
/// through the fallback (forced by the test hook) where `unshare(CLONE_FS)`
/// is refused.
#[test]
fn io_spawn_cwd_failures() {
    let case = std::env::var("LEAN_RUNTIME_TEST_CHILD").unwrap_or_default();
    let base = std::env::temp_dir().join(format!("lean-runtime-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let root = nix::unistd::Uid::effective().is_root();
    match case.as_str() {
        "nosearch" => nosearch_cases(&base, !unshare_allowed()),
        "fallback" => {
            FORCE_FALLBACK.store(true, Ordering::Relaxed);
            cwd_cases(&base);
            if !root {
                nosearch_cases(&base, true);
            }
        }
        // run by hand where `unshare` is refused, e.g. under
        // `systemd-run --user --pipe --wait -p SystemCallFilter=~unshare
        // -p SystemCallErrorNumber=EPERM -E LEAN_RUNTIME_TEST_CHILD=denied`
        "denied" => {
            assert!(!unshare_allowed());
            cwd_cases(&base);
            if !root {
                nosearch_cases(&base, true);
            }
            assert!(NO_PRIVATE_CWD.load(Ordering::Relaxed));
        }
        _ => {
            cwd_cases(&base);
            for c in ["nosearch", "fallback"] {
                if c == "nosearch" && root {
                    continue;
                }
                let out = run_self_as("io::process::tests::io_spawn_cwd_failures", c);
                assert!(out.status.success(), "{c}: {out:?}");
            }
        }
    }
    std::fs::remove_dir_all(&base).unwrap();
}

/// After a spawn with a `cwd`, no thread of the process stays in that
/// directory (leanrs review LRIO2-F1: the spawner goes back to `/`, so
/// `lsof` and `umount` see no use of it).
#[test]
fn spawner_leaves_cwd() {
    let dir = std::env::temp_dir().join(format!("lean-runtime-pin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.to_str().unwrap();
    assert_eq!(pwd_in(d).unwrap().1, format!("{d}\n"));
    let held: Vec<_> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_link(t.ok()?.path().join("cwd")).ok())
        .filter(|c| c.starts_with(&dir))
        .collect();
    assert!(held.is_empty(), "{held:?}");
    std::fs::remove_dir(&dir).unwrap();
}

/// A relative `cwd` with no descriptor left to open `.` (leanrs review
/// LRIO2-F3: `EMFILE`) enters the caller's working directory by its path,
/// as the forked child's `chdir` needs no descriptor; a missing program and
/// a missing directory are the child's failures (a modelled child where the
/// stand-in's pipes cannot be made). Native case `process/spawn_fds_exhausted`;
/// in a child process, with `RLIMIT_NOFILE` at 64.
#[test]
fn spawn_fds_exhausted() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::spawn_fds_exhausted");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    use nix::sys::resource::{setrlimit, Resource};
    setrlimit(Resource::RLIMIT_NOFILE, 64, 64).unwrap();
    let mut hs = Vec::new();
    while let Ok(f) = std::fs::File::open("/dev/null") {
        hs.push(f);
    }
    let c = cfg(Stdio::Inherit, Stdio::Inherit, Stdio::Inherit);
    let run_in = |cmd: &[u8], cwd: Option<&[u8]>| {
        let a = SpawnArgs {
            cwd,
            ..args(cmd, &[])
        };
        spawn(c, &a).and_then(|ch| ch.process.wait())
    };
    assert_eq!(run_in(b"true", Some(b".")).unwrap(), 0);
    assert_eq!(run_in(b"true", Some(b"/")).unwrap(), 0);
    assert_eq!(run_in(MISSING, None).unwrap(), 255);
    assert_eq!(run_in(b"true", Some(b"no-such-dir")).unwrap(), 255);
    // LB-17: a `null` stream cannot be opened: the spawn's error
    let n = cfg(Stdio::Null, Stdio::Inherit, Stdio::Inherit);
    assert_eq!(
        spawn(n, &args(b"true", &[])).unwrap_err(),
        IoError::ResourceExhausted(None, 24, "too many open files".to_owned())
    );
    drop(hs);
}

/// The parent makes every pipe before it opens `/dev/null`, so its pipe
/// ends get the numbers they get natively, where only the forked child opens
/// `/dev/null`: with `null` standard input, piped output and `null` error,
/// the output pipe takes the two lowest free descriptors. In a child process,
/// where no other test opens descriptors meanwhile.
#[test]
fn pipes_before_null() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::pipes_before_null");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    let free: Vec<i32> = {
        let fs: Vec<_> = (0..4)
            .map(|_| std::fs::File::open("/dev/null").unwrap())
            .collect();
        fs.iter().map(|f| f.as_raw_fd()).collect()
    };
    let ends = Ends::new(cfg(Stdio::Null, Stdio::Piped, Stdio::Null)).unwrap();
    let raw = |fd: &Option<OwnedFd>| fd.as_ref().map(|f| f.as_raw_fd());
    assert_eq!(raw(&ends.parent[1]), Some(free[0]));
    assert_eq!(raw(&ends.child[1]), Some(free[1]));
    assert_eq!(raw(&ends.child[0]), Some(free[2]));
    assert_eq!(raw(&ends.child[2]), Some(free[3]));
}

/// LB-15: a `null` stream leaves no `/dev/null` descriptor in the program
/// (native case `process/null_fd_leak`: one per `null` stream). LB-17 (a
/// `null` stream that cannot be opened is the spawn's error, never the
/// parent's stream) is tested in `spawn_fds_exhausted`.
#[test]
fn null_stream_leaks_nothing() {
    let script: &[u8] = b"n=0; for f in /proc/$$/fd/*; do i=${f##*/}; if [ \"$i\" -gt 2 ] && [ \"$(readlink $f)\" = /dev/null ]; then n=$((n+1)); fi; done; echo $n >&2";
    for (i, o) in [(Stdio::Null, Stdio::Null), (Stdio::Piped, Stdio::Null)] {
        let child = spawn(cfg(i, o, Stdio::Piped), &args(b"sh", &[b"-c", script])).unwrap();
        assert_eq!(read_all(child.stderr.as_ref().unwrap()), b"0\n");
        child.process.wait().unwrap();
    }
}

/// A child spawned with a `cwd` (on the spawner thread) gets the calling
/// thread's nice value, as a forked child gets the forking thread's.
#[test]
fn cwd_child_nice() {
    let nice = |cwd: Option<&[u8]>| {
        let a = SpawnArgs {
            cwd,
            ..args(b"sh", &[b"-c", b"cut -d' ' -f19 /proc/self/stat"])
        };
        run(&a, None).unwrap().1
    };
    let mine = rustix::process::getpriority_process(None).unwrap();
    let target = (mine + 3).min(19);
    rustix::process::setpriority_process(None, target).unwrap();
    assert_eq!(nice(None), format!("{target}\n"));
    assert_eq!(nice(Some(b"/")), format!("{target}\n"));
}

/// The nice value a child gets, spawned with `cwd`.
fn child_nice(cwd: Option<&[u8]>) -> String {
    let a = SpawnArgs {
        cwd,
        ..args(b"sh", &[b"-c", b"cut -d' ' -f19 /proc/self/stat"])
    };
    run(&a, None).unwrap().1
}

/// A caller whose nice value is below the one the spawner thread took for
/// another caller (it cannot lower its own without privilege) still gives
/// its child its own, through a helper thread (review RIO2-14: the child got
/// the higher one). In a child process, with a fresh spawner.
#[test]
fn cwd_child_lower_nice() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::cwd_child_lower_nice");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    let mine = rustix::process::getpriority_process(None).unwrap();
    let higher = (mine + 7).min(19);
    let other = std::thread::spawn(move || {
        rustix::process::setpriority_process(None, higher).unwrap();
        child_nice(Some(b"/"))
    })
    .join()
    .unwrap();
    assert_eq!(other, format!("{higher}\n"));
    assert_eq!(child_nice(Some(b"/")), format!("{mine}\n"));
    assert_eq!(child_nice(Some(b".")), format!("{mine}\n"));
    assert_eq!(child_nice(None), format!("{mine}\n"));
}

/// The child's blocked signals are the calling thread's, with a `cwd` too,
/// where the spawner thread (created before the caller blocked the signal)
/// spawns (review RIO2-14). In a child process, with a fresh spawner.
#[test]
fn child_signal_mask() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::child_signal_mask");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    use nix::sys::signal::{SigSet, Signal as Sig};
    let usr1_blocked = |cwd: Option<&[u8]>| {
        let a = SpawnArgs {
            cwd,
            ..args(b"grep", &[b"SigBlk", b"/proc/self/status"])
        };
        let line = run(&a, None).unwrap().1;
        let hex = line.trim().strip_prefix("SigBlk:").unwrap().trim();
        u64::from_str_radix(hex, 16).unwrap() & (1 << (Sig::SIGUSR1 as i32 - 1)) != 0
    };
    // the spawner starts here, with SIGUSR1 unblocked
    assert!(!usr1_blocked(Some(b"/")));
    let mut set = SigSet::empty();
    set.add(Sig::SIGUSR1);
    set.thread_block().unwrap();
    assert!(usr1_blocked(None));
    assert!(usr1_blocked(Some(b"/")));
    set.thread_unblock().unwrap();
    assert!(!usr1_blocked(Some(b"/")));
}

/// In the fallback, a spawn without a `cwd` never runs in another spawn's
/// `cwd`, and a `setCurrentDir` is never undone by a fallback spawn's way
/// back (review RIO2-13: the reviewer's repro saw 125 of 300 such spawns in
/// the wrong directory). In a child process (it changes the working
/// directory), with the fallback forced.
#[test]
fn fallback_cwd_excludes_others() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::fallback_cwd_excludes_others");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    use std::os::unix::ffi::OsStrExt;
    FORCE_FALLBACK.store(true, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("lean-runtime-fbx-{}", std::process::id()));
    let dirs = [base.join("a"), base.join("b")];
    for d in &dirs {
        std::fs::create_dir_all(d).unwrap();
    }
    let dirs = dirs.map(|d| d.canonicalize().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let busy = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                assert_eq!(pwd_in("/").unwrap().1, "/\n");
            }
        })
    };
    for i in 0..100 {
        let d = &dirs[i % 2];
        crate::io::fs::set_current_dir(d.as_os_str().as_bytes()).unwrap();
        let pwd = run(&args(b"pwd", &[]), None).unwrap().1;
        assert_eq!(pwd, format!("{}\n", d.display()), "spawn {i}");
    }
    stop.store(true, Ordering::Relaxed);
    busy.join().unwrap();
    crate::io::fs::set_current_dir(b"/").unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

/// In the fallback, `currentDir`, `getCurrentDir` and `uv_cwd` never read
/// another thread's spawn's `cwd` (review RIO2-20). In a child process (it changes
/// the working directory), with the fallback forced.
#[test]
fn fallback_cwd_reads_excluded() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::fallback_cwd_reads_excluded");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::AtomicUsize;
    FORCE_FALLBACK.store(true, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("lean-runtime-fbr-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    crate::io::fs::set_current_dir(base.as_os_str().as_bytes()).unwrap();
    let spawns = Arc::new(AtomicUsize::new(0));
    let busy = {
        let spawns = spawns.clone();
        std::thread::spawn(move || {
            for _ in 0..100 {
                assert_eq!(pwd_in("/").unwrap().1, "/\n");
                spawns.fetch_add(1, Ordering::Relaxed);
            }
        })
    };
    let want = base.to_str().unwrap();
    let mut reads = 0;
    while spawns.load(Ordering::Relaxed) < 100 {
        let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
        crate::io::fs::current_dir(&mut a).unwrap();
        crate::io::fs::process_current_dir(&mut b).unwrap();
        crate::io::uvsys::cwd(&mut c).unwrap();
        for got in [a, b, c] {
            assert_eq!(String::from_utf8(got).unwrap(), want, "read {reads}");
        }
        reads += 1;
    }
    busy.join().unwrap();
    crate::io::fs::set_current_dir(b"/").unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

/// lean (proc_output): a script without `#!` runs under `/bin/sh`, as
/// `execvp` runs it after `ENOEXEC`, found by its path and through the
/// child's `PATH`; the parent's environment does not change.
#[test]
fn io_process_spawn_script() {
    let dir = std::env::temp_dir().join(format!("lean-runtime-noshebang-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("noshebang");
    std::fs::write(&script, "echo \"script ran with $# args: $*\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = script.to_str().unwrap().as_bytes();
    assert_eq!(
        run_fresh_script(&args(path, &[b"x", b"y"])).1,
        "script ran with 2 args: x y\n"
    );
    let value = format!("/nonexistent:{}", dir.display());
    let env = [(&b"PATH"[..], Some(value.as_bytes()))];
    let a = SpawnArgs {
        env: &env,
        ..args(b"noshebang", &[b"z"])
    };
    assert_eq!(run_fresh_script(&a).1, "script ran with 1 args: z\n");
    assert_ne!(
        std::env::var_os("PATH").unwrap(),
        std::ffi::OsString::from(value)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// lean (A755): a child that cannot start holds the read end of its piped
/// standard input until it exits, which it does at once: input written
/// before then is taken (up to a pipe's capacity), and any write after it
/// fails with `EPIPE` (native case `process/failed_child_stdin`). A small
/// write right after the spawn races with that exit, natively as here.
#[test]
fn io_failed_child_stdin() {
    for n in [0, 2, 4096] {
        let input = vec![b'x'; n];
        match run(&args(MISSING, &[]), Some(&input)) {
            Ok(r) => assert_eq!(r, (255, String::new(), missing_message())),
            Err(e) => assert!(n > 0 && e == broken_pipe(), "{e:?}"),
        }
    }
    for n in [300000, 2 << 20] {
        let input = vec![b'x'; n];
        assert_eq!(
            run(&args(MISSING, &[]), Some(&input)).unwrap_err(),
            broken_pipe()
        );
    }
    let c = cfg(Stdio::Piped, Stdio::Null, Stdio::Piped);
    let child = spawn(c, &args(MISSING, &[])).unwrap();
    until_zombie(child.process.pid());
    let stdin = child.stdin.as_ref().unwrap();
    // a child another test thread is spawning may hold a copy of the read
    // end until its `exec` closes it (close-on-exec): retried
    let mut late = Ok(());
    for _ in 0..100 {
        stdin.put_str(b"late").unwrap();
        late = stdin.flush();
        if late.is_err() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(late.unwrap_err(), broken_pipe());
    assert_eq!(child.process.wait().unwrap(), 255);
}

/// A child that cannot start, with pending standard-output bytes and a piped
/// standard output whose read end the parent has closed: its standard error
/// holds the message alone, as Lean's child fails to write the bytes without
/// a word (review RIO2-17: the stand-in's `cat` reported `write error:
/// Broken pipe`). In a child process, whose standard output (a pipe) holds
/// the pending bytes.
#[test]
fn failed_child_unread_stdout() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::failed_child_unread_stdout");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    Handle::stdout().put_str(b"pending").unwrap();
    for _ in 0..5 {
        let c = cfg(Stdio::Null, Stdio::Piped, Stdio::Piped);
        let mut child = spawn(c, &args(MISSING, &[])).unwrap();
        drop(child.stdout.take());
        let err = String::from_utf8(read_all(child.stderr.as_ref().unwrap())).unwrap();
        assert_eq!(err, missing_message());
        assert_eq!(child.process.wait().unwrap(), 255);
    }
}

/// Waits until process `pid` has exited (a zombie, not yet waited).
fn until_zombie(pid: u32) {
    for _ in 0..1000 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if stat[stat.rfind(')').map_or(0, |i| i + 2)..].starts_with('Z') {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("process {pid} did not exit");
}

/// The rows of a child that cannot start (native case `failed_child_rows`):
/// once it has exited, `kill` succeeds until it is waited, except `killpg`
/// of a child that failed at `chdir` (it never called `setsid`); `wait` and
/// `tryWait` give 255 once.
#[test]
fn failed_child_rows() {
    let c = cfg(Stdio::Null, Stdio::Null, Stdio::Null);
    let mut a = args(MISSING, &[]);
    for (setsid, cwd) in [
        (false, None),
        (true, None),
        (false, Some(&b"no-such-dir"[..])),
        (true, Some(&b"no-such-dir"[..])),
    ] {
        a.setsid = setsid;
        a.cwd = cwd;
        let p = spawn(c, &a).unwrap().process;
        until_zombie(p.pid());
        if setsid && cwd.is_some() {
            assert_eq!(p.kill().unwrap_err(), no_such_process(3));
        } else {
            p.kill().unwrap();
        }
        assert_eq!(p.wait().unwrap(), 255);
        assert_eq!(p.kill().unwrap_err(), no_such_process(3));
        assert_eq!(p.wait().unwrap_err(), no_such_process(10));
        assert_eq!(p.try_wait().unwrap_err(), no_such_process(10));
        // after takeStdin, `kill` is still `killpg` with `setsid` (LB-14)
        let q = spawn(c, &a).unwrap().process.take_stdin();
        until_zombie(q.pid());
        if setsid && cwd.is_some() {
            assert_eq!(q.kill().unwrap_err(), no_such_process(3));
        } else {
            q.kill().unwrap();
        }
        assert_eq!(q.try_wait().unwrap(), Some(255));
    }
}

/// The modelled child that cannot start, where `/bin/sh` cannot be spawned
/// (a test hook forces it; in a child process, since the hook is global): a
/// pid above any the kernel gives, the rows of `failed_child_rows`, its
/// message, and a piped standard input that takes a pipe's capacity until
/// the child is waited, then fails with `EPIPE`.
#[test]
fn modelled_child() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::modelled_child");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    FORCE_NO_SHELL.store(true, Ordering::Relaxed);
    let c = cfg(Stdio::Null, Stdio::Null, Stdio::Piped);
    let mut a = args(MISSING, &[]);
    for (setsid, cwd) in [(false, None), (true, Some(&b"no-such-dir"[..]))] {
        a.setsid = setsid;
        a.cwd = cwd;
        let child = spawn(c, &a).unwrap();
        let p = child.process;
        assert!(p.pid() > 1 << 22, "{}", p.pid());
        let msg = String::from_utf8(read_all(child.stderr.as_ref().unwrap())).unwrap();
        assert!(msg.starts_with("could not "), "{msg}");
        if setsid {
            assert_eq!(p.kill().unwrap_err(), no_such_process(3));
        } else {
            p.kill().unwrap();
        }
        assert_eq!(p.wait().unwrap(), 255);
        assert_eq!(p.kill().unwrap_err(), no_such_process(3));
        assert_eq!(p.try_wait().unwrap_err(), no_such_process(10));
    }
    let c = cfg(Stdio::Piped, Stdio::Null, Stdio::Null);
    let child = spawn(c, &args(MISSING, &[])).unwrap();
    let stdin = child.stdin.as_ref().unwrap();
    stdin.put_str(&[b'y'; 4096]).unwrap();
    stdin.flush().unwrap();
    assert_eq!(
        stdin.put_str(&vec![b'z'; 2 << 20]).unwrap_err(),
        broken_pipe()
    );
    assert_eq!(child.process.wait().unwrap(), 255);
    let stdin = child.stdin.as_ref().unwrap();
    stdin.put_str(b"late").unwrap();
    assert_eq!(stdin.flush().unwrap_err(), broken_pipe());
}

/// glibc's `execvp` search: an empty name, `EACCES` remembered while the
/// search goes on, an entry that is a file (`ENOTDIR`), an empty entry (the
/// working directory, after `cwd`), a name longer than `NAME_MAX` (native
/// case `exec_search`).
#[test]
fn exec_search() {
    let dir = std::env::temp_dir().join(format!("lean-runtime-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["bin1", "bin2"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join("bin1/tool"), "echo from bin1\n").unwrap();
    std::fs::set_permissions(
        dir.join("bin1/tool"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(dir.join("bin2/tool"), "echo \"from bin2 $# $*\"\n").unwrap();
    std::fs::set_permissions(
        dir.join("bin2/tool"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(dir.join("plain"), "x").unwrap();
    let top = dir.to_str().unwrap();
    let with_path = |cmd: &[u8], path: &str, cwd: Option<&[u8]>| {
        let env = [(&b"PATH"[..], Some(path.as_bytes()))];
        let a = SpawnArgs {
            env: &env,
            cwd,
            ..args(cmd, &[b"a"])
        };
        run_fresh_script(&a)
    };
    let not_found = |cmd: &str| {
        (
            255,
            String::new(),
            format!("could not execute external process '{cmd}'\n"),
        )
    };
    assert_eq!(run(&args(b"", &[]), None).unwrap(), not_found(""));
    let both = format!("{top}/bin1:{top}/bin2");
    assert_eq!(with_path(b"tool", &both, None).1, "from bin2 1 a\n");
    if !nix::unistd::Uid::effective().is_root() {
        assert_eq!(
            with_path(b"tool", &format!("{top}/bin1"), None),
            not_found("tool")
        );
    }
    assert_eq!(
        with_path(b"tool", &format!("{top}/plain:{top}/bin2"), None).1,
        "from bin2 1 a\n"
    );
    let bin2 = format!("{top}/bin2");
    assert_eq!(
        with_path(b"tool", ":/nonexistent", Some(bin2.as_bytes())).1,
        "from bin2 1 a\n"
    );
    assert_eq!(
        with_path(b"tool", "", Some(bin2.as_bytes())).1,
        "from bin2 1 a\n"
    );
    let long = "n".repeat(300);
    assert_eq!(
        run(&args(long.as_bytes(), &[]), None).unwrap(),
        not_found(&long)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The child's environment: the parent's entries in order, then the changes
/// as glibc's `setenv` and `unsetenv` apply them (a name they refuse changes
/// nothing; names and values are C strings); `inheritEnv := false` starts
/// empty. The parent's environment does not change.
#[test]
fn child_environment() {
    let env_of = |changes: &[(&[u8], Option<&[u8]>)], inherit: bool| {
        let a = SpawnArgs {
            env: changes,
            inherit_env: inherit,
            ..args(b"/usr/bin/env", &[])
        };
        run(&a, None).unwrap().1
    };
    assert_eq!(
        env_of(&[(b"ONLY", Some(b"this one"))], false),
        "ONLY=this one\n"
    );
    assert_eq!(
        env_of(
            &[
                (b"A", Some(b"1")),
                (b"B", Some(b"2")),
                (b"A", Some(b"3")),
                (b"", Some(b"x")),
                (b"C=D", Some(b"y")),
                (b"E\0F", Some(b"v\0w"))
            ],
            false
        ),
        "A=3\nB=2\nE=v\n"
    );
    assert_eq!(env_of(&[(b"A", Some(b"1")), (b"A", None)], false), "");
    let parent = environ::entries();
    let inherited = env_of(&[], true);
    let expect: String = parent
        .iter()
        .map(|e| format!("{}\n", String::from_utf8_lossy(e)))
        .collect();
    assert_eq!(inherited, expect);
    assert_eq!(environ::entries(), parent);
}

// ---------------------------------------------------------------------------
// Threads mode: the working directory's rule (docs/threads.md, 3.2; review
// RT1-04)

/// Threads mode, where a spawn takes the fallback (forced by the test hook):
/// another thread's path operations (`metadata`, `Handle.mk`, `readDir`,
/// `realPath`, `createDir`, `removeDir` of relative paths, and `metadata`
/// of an absolute path through `/proc/self/cwd`) never resolve against the
/// spawn's `cwd`, since they hold `CWD_LOCK` shared (`with_path_lookup`).
/// Before T2 they took no lock, and the first look saw the spawn's `cwd` (a
/// missing `marker`). In a child process (it changes the working
/// directory), with the fallback forced.
#[cfg(feature = "threads")]
#[test]
#[cfg_attr(miri, ignore)]
fn threads_path_lookups_never_see_a_fallback_spawns_cwd() {
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out =
            run_self("io::process::tests::threads_path_lookups_never_see_a_fallback_spawns_cwd");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::AtomicUsize;
    FORCE_FALLBACK.store(true, Ordering::Relaxed);
    // as `sched::start` does: the spawn path stays the fallback
    decide_spawn_path();
    assert!(fallback_possible());
    let base = std::env::temp_dir().join(format!("lean-runtime-rt104-{}", std::process::id()));
    let (here, there) = (base.join("here"), base.join("there"));
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&there).unwrap();
    std::fs::write(here.join("marker"), "here").unwrap();
    let here = here.canonicalize().unwrap();
    let there = there.canonicalize().unwrap();
    crate::io::fs::set_current_dir(here.as_os_str().as_bytes()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let looks = Arc::new(AtomicUsize::new(0));
    let busy = {
        let (stop, looks, here) = (stop.clone(), looks.clone(), here.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let n = looks.fetch_add(1, Ordering::Relaxed);
                assert!(crate::io::fs::metadata(b"marker").is_ok(), "metadata {n}");
                assert!(
                    crate::io::fs::metadata(b"/proc/self/cwd/marker").is_ok(),
                    "metadata of /proc/self/cwd/marker {n}"
                );
                let h = Handle::open(b"marker", FsMode::Read).expect("Handle.mk");
                let mut b = [0u8; 4];
                assert_eq!(h.read(&mut b).unwrap(), 4, "read {n}");
                assert_eq!(&b, b"here");
                drop(h);
                let mut names = Vec::new();
                crate::io::fs::read_dir(b".", |e| names.push(e.to_vec())).unwrap();
                assert!(names.iter().any(|e| e == b"marker"), "readDir {n}");
                let mut real = Vec::new();
                crate::io::fs::real_path(b"marker", &mut real).unwrap();
                assert_eq!(
                    real,
                    here.join("marker").as_os_str().as_bytes(),
                    "realPath {n}"
                );
                crate::io::fs::create_dir(b"sub").unwrap();
                crate::io::fs::remove_dir(b"sub").unwrap();
            }
        })
    };
    for i in 0..100 {
        let got = pwd_in(there.to_str().unwrap()).unwrap().1;
        assert_eq!(got, format!("{}\n", there.display()), "spawn {i}");
    }
    stop.store(true, Ordering::Relaxed);
    busy.join().unwrap();
    assert!(looks.load(Ordering::Relaxed) > 0);
    assert!(!there.join("sub").exists());
    crate::io::fs::set_current_dir(b"/").unwrap();
    std::fs::remove_dir_all(&base).unwrap();
}

/// Threads mode, where `unshare(CLONE_FS)` works: `sched::start` starts the
/// spawner, from then on no spawn takes the fallback (a fallback spawn is
/// refused with `EAGAIN`, and the working directory stays), and relative
/// path operations take no lock. In a child process (the decision is the
/// process's for good).
#[cfg(feature = "threads")]
#[test]
#[cfg_attr(miri, ignore)]
fn threads_no_fallback_once_the_spawner_unshared() {
    if !unshare_allowed() {
        eprintln!("note: unshare(CLONE_FS) is refused here: not checked");
        return;
    }
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self("io::process::tests::threads_no_fallback_once_the_spawner_unshared");
        assert!(out.status.success(), "{out:?}");
        return;
    }
    decide_spawn_path();
    assert!(!fallback_possible());
    let here = std::env::current_dir().unwrap();
    let spec = Spec::new(&args(b"true", &[]));
    let r = fallback_spawn(&spec, &[], b"/", false);
    assert!(matches!(r, Err(SpawnError::Os(EAGAIN))), "{r:?}");
    assert_eq!(std::env::current_dir().unwrap(), here);
    // the spawner still enters `cwd` for its spawns
    assert_eq!(pwd_in("/").unwrap().1, "/\n");
}

/// Runs this test binary's `test` in a child process with
/// `LEAN_RUNTIME_TEST_CHILD=1`; kills it after `secs` seconds and fails if
/// it had not ended by then (a deadlock).
#[cfg(feature = "threads")]
fn run_self_within(test: &str, secs: u64) {
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env("LEAN_RUNTIME_TEST_CHILD", "1")
        .spawn()
        .unwrap();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            assert!(st.success(), "{test}: {st:?}");
            return;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{test}: the child did not end within {secs} s (a deadlock)");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// RT2-03 (review of threads-2): where a spawn takes the fallback, a path
/// lookup that blocks must not hold `CWD_LOCK`. Opening a FIFO waits for
/// its other end; a spawn with a `cwd` whose child is that other end waits
/// for the exclusive lock, and (std's lock prefers a waiting writer) so
/// does every later path operation. So `Handle.mk` only resolves the path
/// under the lock (`O_PATH`), and opens it after the unlock. Here a reader
/// (`Handle.mk .read`) and a creating writer (`Handle.mk .write`, which
/// natively blocks on an existing FIFO too) wait in their opens while
/// fallback spawns' children open the other ends. Before the fix: a
/// deadlock (killed at 10 s); natively, and after, both go on. In a child
/// process, with the fallback forced.
#[cfg(feature = "threads")]
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_03_a_fifo_open_does_not_block_a_fallback_spawn() {
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;
    let name = "io::process::tests::rt2_03_a_fifo_open_does_not_block_a_fallback_spawn";
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        run_self_within(name, 10);
        return;
    }
    FORCE_FALLBACK.store(true, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("lean-runtime-rt2-fifo-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    for f in ["f", "g"] {
        assert!(std::process::Command::new("mkfifo")
            .arg(base.join(f))
            .status()
            .unwrap()
            .success());
    }
    let dir = base.as_os_str().as_bytes();
    // a reader waits in its open
    let f = base.join("f").as_os_str().as_bytes().to_vec();
    let reader = std::thread::spawn(move || {
        let h = Handle::open(&f, FsMode::Read).expect("open the FIFO");
        read_all(&h)
    });
    std::thread::sleep(Duration::from_millis(300));
    let r = run(
        &SpawnArgs {
            cwd: Some(dir),
            ..args(b"sh", &[b"-c", b"echo hi > f"])
        },
        None,
    )
    .unwrap();
    assert_eq!(r.0, 0, "{r:?}");
    assert_eq!(reader.join().unwrap(), b"hi\n");
    // a creating writer waits in its open
    let g = base.join("g").as_os_str().as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let h = Handle::open(&g, FsMode::Write).expect("open the FIFO");
        h.put_str(b"there\n").unwrap();
        h.flush().unwrap();
    });
    std::thread::sleep(Duration::from_millis(300));
    let r = run(
        &SpawnArgs {
            cwd: Some(dir),
            ..args(b"cat", &[b"g"])
        },
        None,
    )
    .unwrap();
    writer.join().unwrap();
    assert_eq!(r, (0, "there\n".to_owned(), String::new()));
    std::fs::remove_dir_all(&base).unwrap();
}

/// Threads mode, where a spawn takes the fallback: `Handle.mk`'s open in
/// two steps (the path resolved under `CWD_LOCK`, the file opened after
/// through `/proc/self/fd`; review RT2-03) gives what the one `open(2)`
/// gives, in each mode: the same errors (a missing file, an existing one
/// with `writeNew`, a directory opened for writing, a file without
/// permission), and the same files (created, truncated, appended to, the
/// target of a dangling symbolic link created). Each case runs in two
/// directories set up alike: once through `Handle::open`, once through the
/// one `open(2)`. In a child process, with the fallback forced.
#[cfg(feature = "threads")]
#[test]
#[cfg_attr(miri, ignore)]
fn threads_fallback_opens_keep_their_outcomes() {
    use std::os::unix::ffi::OsStrExt;
    let name = "io::process::tests::threads_fallback_opens_keep_their_outcomes";
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        run_self_within(name, 60);
        return;
    }
    FORCE_FALLBACK.store(true, Ordering::Relaxed);
    assert!(fallback_possible());
    let root = nix::unistd::Uid::effective().is_root();
    let base = std::env::temp_dir().join(format!("lean-runtime-rt2-opens-{}", std::process::id()));
    let setup = |d: &std::path::Path| {
        std::fs::create_dir_all(d.join("dir")).unwrap();
        std::fs::write(d.join("old"), "old\n").unwrap();
        std::fs::write(d.join("locked"), "x").unwrap();
        std::fs::set_permissions(d.join("locked"), std::fs::Permissions::from_mode(0o200)).unwrap();
        std::os::unix::fs::symlink(d.join("target"), d.join("dangling")).unwrap();
    };
    let (a, b) = (base.join("a"), base.join("b"));
    setup(&a);
    setup(&b);
    let modes = [
        FsMode::Read,
        FsMode::Write,
        FsMode::WriteNew,
        FsMode::ReadWrite,
        FsMode::Append,
    ];
    for file in ["new", "old", "dir", "locked", "dangling", "missing/x"] {
        for &mode in &modes {
            let (pa, pb) = (a.join(file), b.join(file));
            let got = Handle::open(pa.as_os_str().as_bytes(), mode).map(|h| {
                let _ = h.put_str(b"w\n");
                let _ = h.flush();
            });
            let want = rustix::fs::open(
                &pb,
                mode.open_flags(),
                rustix::fs::Mode::from_raw_mode(0o666),
            )
            .map(|fd| {
                let _ = rustix::io::write(&fd, b"w\n");
            });
            let label = format!("{file} {mode:?}");
            match (&got, &want) {
                (Ok(()), Ok(())) => {}
                (Err(e), Err(w)) => assert_eq!(
                    *e,
                    IoError::decode_io_error(w.raw_os_error(), Some(pa.as_os_str().as_bytes())),
                    "{label}"
                ),
                _ if root && file == "locked" => {}
                _ => panic!("{label}: {got:?} where the one open gives {want:?}"),
            }
        }
    }
    // the files are alike
    for file in ["new", "old", "locked", "target"] {
        assert_eq!(
            std::fs::read(a.join(file)).ok(),
            std::fs::read(b.join(file)).ok(),
            "{file}"
        );
    }
    // and so are the open files' status flags: a creating open's added
    // `O_NONBLOCK` is cleared, its `O_APPEND` kept (review RT2-11)
    for &mode in &modes[..] {
        if mode == FsMode::WriteNew {
            continue;
        }
        let flags = mode.open_flags();
        let m = rustix::fs::Mode::from_raw_mode(0o666);
        let ours = open_looked_up(&a.join("old"), flags, m).unwrap();
        let one = rustix::fs::open(b.join("old"), flags, m).unwrap();
        assert_eq!(
            rustix::fs::fcntl_getfl(&ours).unwrap(),
            rustix::fs::fcntl_getfl(&one).unwrap(),
            "{mode:?}"
        );
    }
    std::fs::remove_dir_all(&base).unwrap();
}

/// Review RT2-12: in the fallback regime `Handle.mk`'s open needs no more
/// descriptors than the one `open(2)`, and its file gets the number the one
/// open would get. Before the fix the two-step open held the `O_PATH`
/// descriptor while it reopened: with one descriptor left it failed with
/// `EMFILE` where the one open succeeds, and otherwise the file got the
/// second-lowest free number. In a child process, with the fallback forced.
#[cfg(feature = "threads")]
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_12_open_at_the_descriptor_limit() {
    let name = "io::process::tests::rt2_12_open_at_the_descriptor_limit";
    if std::env::var_os("LEAN_RUNTIME_TEST_CHILD").is_none() {
        let out = run_self(name);
        assert!(out.status.success(), "{out:?}");
        return;
    }
    use rustix::fs::{Mode, OFlags};
    FORCE_FALLBACK.store(true, Ordering::Relaxed);
    let file = std::env::current_exe().unwrap();
    let path = file.as_os_str().as_encoded_bytes().to_vec();
    let flags = FsMode::Read.open_flags();
    // the number of the one open(2), then of the two-step one
    let one = rustix::fs::open(&file, flags, Mode::empty()).unwrap();
    let n_one = one.as_raw_fd();
    drop(one);
    let two = open_looked_up(&file, flags, Mode::empty()).unwrap();
    assert_eq!(two.as_raw_fd(), n_one, "the file's number");
    drop(two);
    // one descriptor left
    let lim = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    rustix::process::setrlimit(
        rustix::process::Resource::Nofile,
        rustix::process::Rlimit {
            current: Some(64),
            maximum: lim.maximum,
        },
    )
    .unwrap();
    let mut fill = Vec::new();
    while let Ok(f) = rustix::fs::open("/dev/null", OFlags::RDONLY, Mode::empty()) {
        fill.push(f);
    }
    fill.pop();
    let direct = rustix::fs::open(&file, flags, Mode::empty());
    assert!(direct.is_ok());
    drop(direct);
    let via = Handle::open(&path, FsMode::Read);
    assert!(
        via.is_ok(),
        "Handle.mk with one descriptor left: {:?}",
        via.err()
    );
    drop(via);
    drop(fill);
}
