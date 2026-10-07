//! Row tests for `lean_runtime::io` against native Lean 4.34.0. Every expected
//! value is an output of the compiled-Lean probe `tests/data/io_probe`
//! (`run.sh` rebuilds it and rewrites its `.txt` outputs; `Main.lean` names
//! each mode): a test rebuilds the probe's scenario in a fresh directory,
//! prints the outcome in the probe's format and compares it with the probe's
//! line, the probe's directory replaced by the test's. Values the probe
//! printed outside those files are quoted at their test as `// lean (probe
//! <mode>):`.
//!
//! Tests that need a process of their own (exit paths, standard input, the
//! environment, the working directory, the block policy, locks) re-run the
//! test binary as a child. Miri cannot spawn processes or use the file
//! system, so the whole file is skipped under Miri.
//!
//! Ported from leanrs's `rt/leanrs_rt/tests/io.rs` (the probe and most
//! scenarios) to this crate's API: byte views for paths, results written into
//! the caller's buffer, `IoError` with `String` fields. Lean's lossy decoding
//! of names and lines is a translator's, so those rows compare bytes.
#![cfg(all(feature = "io", not(miri)))]

use std::ffi::OsStr;
use std::fs;
use std::io::{IsTerminal, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use lean_runtime::io::fs::{self as lfs, FileType, Metadata};
use lean_runtime::io::{env, exit, FsMode, Handle, IoError};

const PROBE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/io_probe");
/// The directory the probe's `fs` mode ran in.
const PROBE_DIR: &str = "/tmp/lean-runtime-io-probe";
const CHILD_VAR: &str = "LEAN_RUNTIME_TEST_CHILD";

fn data(file: &str) -> String {
    fs::read_to_string(format!("{PROBE}/{file}")).expect("probe output")
}

// ---- the probe's output format (`Main.lean`: `desc`, `r`) ----

/// Lean's `String.quote` (`Char.quoteCore` in a string).
fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if (c as u32) <= 31 || c == '\x7f' => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt(o: &Option<String>) -> String {
    match o {
        None => "None".to_owned(),
        Some(s) => format!("Some({})", q(s)),
    }
}

fn desc(e: &IoError) -> String {
    use IoError as E;
    match e {
        E::AlreadyExists(f, c, d) => format!("AlreadyExists({}, {c}, {})", opt(f), q(d)),
        E::OtherError(c, d) => format!("OtherError({c}, {})", q(d)),
        E::ResourceBusy(c, d) => format!("ResourceBusy({c}, {})", q(d)),
        E::ResourceVanished(c, d) => format!("ResourceVanished({c}, {})", q(d)),
        E::UnsupportedOperation(c, d) => format!("UnsupportedOperation({c}, {})", q(d)),
        E::HardwareFault(c, d) => format!("HardwareFault({c}, {})", q(d)),
        E::UnsatisfiedConstraints(c, d) => format!("UnsatisfiedConstraints({c}, {})", q(d)),
        E::IllegalOperation(c, d) => format!("IllegalOperation({c}, {})", q(d)),
        E::ProtocolError(c, d) => format!("ProtocolError({c}, {})", q(d)),
        E::TimeExpired(c, d) => format!("TimeExpired({c}, {})", q(d)),
        E::Interrupted(f, c, d) => format!("Interrupted({}, {c}, {})", q(f), q(d)),
        E::NoFileOrDirectory(f, c, d) => format!("NoFileOrDirectory({}, {c}, {})", q(f), q(d)),
        E::InvalidArgument(f, c, d) => format!("InvalidArgument({}, {c}, {})", opt(f), q(d)),
        E::PermissionDenied(f, c, d) => format!("PermissionDenied({}, {c}, {})", opt(f), q(d)),
        E::ResourceExhausted(f, c, d) => format!("ResourceExhausted({}, {c}, {})", opt(f), q(d)),
        E::InappropriateType(f, c, d) => format!("InappropriateType({}, {c}, {})", opt(f), q(d)),
        E::NoSuchThing(f, c, d) => format!("NoSuchThing({}, {c}, {})", opt(f), q(d)),
        E::UnexpectedEof => "UnexpectedEof".to_owned(),
        E::UserError(m) => format!("UserError({})", q(m)),
    }
}

fn r<T>(label: &str, res: Result<T, IoError>, show: impl FnOnce(T) -> String) -> String {
    match res {
        Ok(v) => format!("{label}: ok {}", show(v)),
        Err(e) => format!("{label}: err {}", desc(&e)),
    }
}

fn unit(_: ()) -> String {
    "()".to_owned()
}

fn file_type(t: FileType) -> &'static str {
    match t {
        FileType::Dir => "IO.FS.FileType.dir",
        FileType::File => "IO.FS.FileType.file",
        FileType::Symlink => "IO.FS.FileType.symlink",
        FileType::Other => "IO.FS.FileType.other",
    }
}

fn show_type(m: Metadata) -> String {
    format!("type={}", file_type(m.file_type))
}

/// The probe's `fs.txt` line for `label`, its directory replaced by `dir`.
fn lean(label: &str, dir: &str) -> String {
    let text = data("fs.txt");
    let prefix = format!("{label}: ");
    let line = text
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no probe line {label}"));
    line.replace(PROBE_DIR, dir)
}

#[track_caller]
fn check(dir: &str, got: String) {
    let label = got.split(": ").next().unwrap_or_default().to_owned();
    assert_eq!(got, lean(&label, dir));
}

// ---- the crate's API as a Lean program sees it ----

fn s(text: &str) -> &[u8] {
    text.as_bytes()
}

fn open(path: &str, mode: FsMode) -> Result<Handle, IoError> {
    Handle::open(path.as_bytes(), mode)
}

fn put(h: &Handle, text: &str) -> Result<(), IoError> {
    h.put_str(text.as_bytes())
}

/// `Handle.read n`: the size check, Lean's allocation of `n` bytes, the read.
fn read(h: &Handle, n: usize) -> Result<Vec<u8>, IoError> {
    lean_runtime::io::handle::check_read_size(n)?;
    let mut v = vec![0u8; n];
    let got = h.read(&mut v)?;
    v.truncate(got);
    Ok(v)
}

/// `Handle.getLine` (as text: the test lines are UTF-8).
fn get_line(h: &Handle) -> Result<String, IoError> {
    let mut v = Vec::new();
    h.get_line(&mut v)?;
    Ok(String::from_utf8(v).expect("a UTF-8 line"))
}

fn get_line_bytes(h: &Handle) -> Result<Vec<u8>, IoError> {
    let mut v = Vec::new();
    h.get_line(&mut v).map(|()| v)
}

fn quoted_list(v: &[String]) -> String {
    let items: Vec<String> = v.iter().map(|x| q(x)).collect();
    format!("[{}]", items.join(", "))
}

// ---- scenario set-up ----

/// A test's directory, removed when the test ends.
struct TestDir(String);

impl std::ops::Deref for TestDir {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TestDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fresh directory laid out as the probe's: `full/f` holding `x`, the
/// symlinks `link` (to `full/f`) and `dangling`, `names/` with two names that
/// are not UTF-8, and `bytes.txt`.
fn setup(name: &str) -> TestDir {
    let d = std::env::temp_dir().join(format!("lean-runtime-io-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("full")).unwrap();
    fs::write(d.join("full/f"), "x").unwrap();
    std::os::unix::fs::symlink("full/f", d.join("link")).unwrap();
    std::os::unix::fs::symlink("nowhere", d.join("dangling")).unwrap();
    fs::create_dir(d.join("names")).unwrap();
    for n in [&b"a\xffb"[..], &b"\xc0\x80"[..]] {
        fs::write(d.join("names").join(OsStr::from_bytes(n)), "").unwrap();
    }
    fs::write(d.join("bytes.txt"), b"ok\n\xc0\x80x\n\xe2\x82").unwrap();
    TestDir(d.to_str().unwrap().to_owned())
}

fn p(dir: &str, rest: &str) -> String {
    format!("{dir}/{rest}")
}

// ---- child processes ----

fn child_case() -> Option<String> {
    std::env::var(CHILD_VAR).ok()
}

/// Runs test `test` again as a child with `CHILD_VAR=case`, after `setup`
/// adjusts the command.
fn child(test: &str, case: &str, setup: impl FnOnce(&mut Command)) -> Output {
    let mut c = Command::new(std::env::current_exe().expect("test binary path"));
    c.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_VAR, case)
        .stdin(Stdio::null());
    setup(&mut c);
    c.output().expect("child test process")
}

/// The same with `input` on the child's standard input.
fn child_with_stdin(test: &str, case: &str, input: &[u8]) -> Output {
    let mut c = Command::new(std::env::current_exe().expect("test binary path"));
    c.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_VAR, case)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proc = c.spawn().expect("child test process");
    // the inputs are a few bytes, far below one page (the smallest pipe)
    assert!(input.len() < 4096);
    proc.stdin
        .take()
        .expect("stdin pipe")
        .write_all(input)
        .unwrap();
    proc.wait_with_output().expect("child output")
}

/// The lines a child wrote after `RESULT: `.
fn results(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("RESULT: ").map(str::to_owned))
        .collect()
}

/// In a child: writes `RESULT: <line>` for each line through the stdout
/// handle, then exits 0 as Lean does (the streams flushed). The first line
/// starts on a fresh line, after the harness's `test <name> ... `.
fn report(lines: &[String]) -> ! {
    let h = Handle::stdout();
    put(&h, "\n").unwrap();
    for l in lines {
        put(&h, &format!("RESULT: {l}\n")).unwrap();
    }
    exit::exit(0)
}

// ---- errors ----

/// The decoder against Lean's `lean_decode_io_error` for every `errno` of the
/// probe (0 to 140 and four larger codes), without and with a path. The probe
/// skips 2 and 4 without a path, where native crashes (LB-03). Row 74,
/// `EBADMSG`, is the corrected class, not the probe's native one (LB-47:
/// native gives `protocolError`, where IOError.lean places it with
/// `inappropriateType`; `src/io/error_tests.rs` row 74 does the same).
#[test]
fn io_error_classes() {
    let text = data("errno.txt");
    let mut n = 0;
    for line in text.lines() {
        let (e, rest) = line.split_once(": ").unwrap();
        let (none, some) = rest.split_once(" | ").unwrap();
        let e: i32 = e.parse().unwrap();
        let (none, some) = if e == 74 {
            assert_eq!(
                (none, some),
                (
                    r#"ProtocolError(74, "protocol error")"#,
                    r#"ProtocolError(74, "protocol error")"#
                ),
                "the probe's native row 74"
            );
            (
                r#"InappropriateType(None, 74, "protocol error")"#,
                r#"InappropriateType(Some("p"), 74, "protocol error")"#,
            )
        } else {
            (none, some)
        };
        if none != "-" {
            assert_eq!(desc(&IoError::decode_io_error(e, None)), none, "errno {e}");
        }
        assert_eq!(
            desc(&IoError::decode_io_error(e, Some(b"p"))),
            some,
            "errno {e}"
        );
        n += 1;
    }
    assert_eq!(n, 145);
    // LB-03: the classes that need a name get "" without one
    assert_eq!(
        desc(&IoError::decode_io_error(2, None)),
        r#"NoFileOrDirectory("", 2, "no such file or directory")"#
    );
    assert_eq!(
        desc(&IoError::decode_io_error(4, None)),
        r#"Interrupted("", 4, "interrupted system call")"#
    );
}

// ---- handles ----

#[test]
fn io_handle_mk() {
    let d = setup("mk");
    check(&d, r("mk nul", open("a\0b", FsMode::Read), |_| "h".into()));
    check(
        &d,
        r("mk missing", open("/nonexistent/x", FsMode::Read), |_| {
            "h".into()
        }),
    );
    check(
        &d,
        r(
            "mk proc write",
            open("/proc/version", FsMode::Write),
            |_| "h".into(),
        ),
    );
    check(
        &d,
        r("dir write", open(&p(&d, "full"), FsMode::Write), |_| {
            "h".into()
        }),
    );
    check(
        &d,
        r(
            "writeNew exists",
            open(&p(&d, "full/f"), FsMode::WriteNew),
            |_| "h".into(),
        ),
    );
    check(
        &d,
        r(
            "readWrite missing",
            open(&p(&d, "rwmissing"), FsMode::ReadWrite),
            |_| "h".into(),
        ),
    );
    // write truncates, writeNew creates
    fs::write(p(&d, "t"), "old").unwrap();
    drop(open(&p(&d, "t"), FsMode::Write).unwrap());
    assert_eq!(fs::read(p(&d, "t")).unwrap(), b"");
    let h = open(&p(&d, "new"), FsMode::WriteNew).unwrap();
    put(&h, "n").unwrap();
    drop(h);
    assert_eq!(fs::read_to_string(p(&d, "new")).unwrap(), "n");
}

#[test]
fn io_handle_get_line() {
    let d = setup("get_line");
    let f = p(&d, "h.txt");
    fs::write(&f, "line1\nline2\nlast").unwrap();
    let seq = (|| {
        let h = open(&f, FsMode::Read)?;
        (0..5)
            .map(|_| get_line(&h))
            .collect::<Result<Vec<String>, IoError>>()
    })();
    check(&d, r("getLine seq", seq, |v| quoted_list(&v)));
    let dir_read = open(&p(&d, "full"), FsMode::Read).and_then(|h| get_line(&h));
    check(&d, r("dir read", dir_read, |l| q(&l)));
    let w = open(&p(&d, "w.txt"), FsMode::Write).and_then(|h| get_line(&h));
    check(&d, r("getLine on write", w, |l| q(&l)));
    let a = open(&p(&d, "w.txt"), FsMode::Append).and_then(|h| get_line(&h));
    check(&d, r("getLine on append", a, |l| q(&l)));
    check(
        &d,
        r("stdout getLine", get_line(&Handle::stdout()), |l| q(&l)),
    );
    // lean (probe fs, `getLine lossy`): the lines' bytes before Lean's lossy
    // decoding, which is the translator's
    let h = open(&p(&d, "bytes.txt"), FsMode::Read).unwrap();
    let lines: Vec<Vec<u8>> = (0..3).map(|_| get_line_bytes(&h).unwrap()).collect();
    assert_eq!(lines, [&b"ok\n"[..], b"\xc0\x80x\n", b"\xe2\x82"]);
    // a line longer than the buffer
    let long = "y".repeat(20000);
    fs::write(&f, format!("{long}\nz")).unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(get_line(&h).unwrap(), format!("{long}\n"));
    assert_eq!(get_line(&h).unwrap(), "z");
}

#[test]
fn io_handle_read() {
    let d = setup("read");
    let f = p(&d, "h.txt");
    fs::write(&f, "line1\nline2\nlast").unwrap();
    let seq = (|| {
        let h = open(&f, FsMode::Read)?;
        [0, 3, 100, 5]
            .iter()
            .map(|&n| read(&h, n).map(|b| b.len()))
            .collect::<Result<Vec<usize>, IoError>>()
    })();
    check(&d, r("read seq", seq, |v| format!("{v:?}")));
    let dir = open(&p(&d, "full"), FsMode::Read).and_then(|h| read(&h, 10));
    check(&d, r("dir readbytes", dir, |b| b.len().to_string()));
    let w = open(&p(&d, "w.txt"), FsMode::Write).and_then(|h| read(&h, 4));
    check(&d, r("read on write", w, |b| b.len().to_string()));
    let big = open(&f, FsMode::Read).and_then(|h| read(&h, usize::MAX));
    check(&d, r("read overflow", big, |b| b.len().to_string()));
    check(
        &d,
        r("stdout read", read(&Handle::stdout(), 4), |b| {
            b.len().to_string()
        }),
    );
    // `usize::MAX - 24` is the largest size whose byte array does not overflow
    assert!(lean_runtime::io::handle::check_read_size(usize::MAX - 23).is_err());
    assert!(lean_runtime::io::handle::check_read_size(usize::MAX - 24).is_ok());
}

#[test]
fn io_handle_put_str() {
    let d = setup("put_str");
    let f = p(&d, "h.txt");
    fs::write(&f, "line1").unwrap();
    let ro = open(&f, FsMode::Read).and_then(|h| put(&h, "x"));
    check(&d, r("write on read", ro, unit));
    check(&d, r("stdin write", put(&Handle::stdin(), "x"), unit));
    let rw = (|| {
        fs::write(&f, "abcdef\nghij\n").unwrap();
        let h = open(&f, FsMode::ReadWrite)?;
        let a = read(&h, 2)?;
        put(&h, "XY")?;
        let b = get_line(&h)?;
        put(&h, "Z")?;
        h.rewind()?;
        let c = get_line(&h)?;
        let c2 = get_line(&h)?;
        Ok(format!("{}|{b}|{c}|{c2}", a.len()))
    })();
    check(&d, r("rw interleave", rw, |t| q(&t)));
    check(
        &d,
        r(
            "rw file",
            Ok::<_, IoError>(fs::read_to_string(&f).unwrap()),
            |t| q(&t),
        ),
    );
}

#[test]
fn io_handle_write() {
    let d = setup("write");
    let f = p(&d, "bin");
    // lean (probe fs, `readBinFile` of bytes.txt): [111, 107, 10, 192, 128, 120, 10, 226, 130]
    let bytes = [111u8, 107, 10, 192, 128, 120, 10, 226, 130];
    let h = open(&f, FsMode::Write).unwrap();
    h.write(&bytes).unwrap();
    h.write(&[]).unwrap();
    drop(h);
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(read(&h, 100).unwrap(), bytes);
    let ro = open(&f, FsMode::Read).and_then(|h| h.write(b"x"));
    assert_eq!(r("write on read", ro, unit), lean("write on read", &d));
}

#[test]
fn io_handle_flush() {
    let d = setup("flush");
    let f = p(&d, "h.txt");
    fs::write(&f, "line1").unwrap();
    let ro = open(&f, FsMode::Read).and_then(|h| {
        put(&h, "x")?;
        h.flush()
    });
    check(&d, r("write on read flush", ro, unit));
    check(&d, r("stdout flush", Handle::stdout().flush(), unit));
    let h = open(&f, FsMode::Write).unwrap();
    put(&h, "abc").unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"");
    h.flush().unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"abc");
    // a flush of a reading handle gives the read-ahead back; the next read is unchanged
    fs::write(&f, "one\ntwo\n").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(get_line(&h).unwrap(), "one\n");
    h.flush().unwrap();
    assert_eq!(get_line(&h).unwrap(), "two\n");
}

#[test]
fn io_handle_rewind() {
    let d = setup("rewind");
    let f = p(&d, "h.txt");
    let app = (|| {
        fs::write(&f, "ab").unwrap();
        let h = open(&f, FsMode::Append)?;
        put(&h, "cd")?;
        h.rewind()?;
        put(&h, "ef")?;
        h.flush()?;
        Ok(fs::read_to_string(&f).unwrap())
    })();
    check(&d, r("append", app, |t| q(&t)));
    fs::write(&f, "abc\n").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(get_line(&h).unwrap(), "abc\n");
    h.rewind().unwrap();
    assert_eq!(get_line(&h).unwrap(), "abc\n");
}

#[test]
fn io_handle_truncate() {
    let d = setup("truncate");
    let f = p(&d, "h.txt");
    let t1 = (|| {
        fs::write(&f, "0123456789").unwrap();
        let h = open(&f, FsMode::ReadWrite)?;
        let _ = read(&h, 4)?;
        h.truncate()?;
        let t = get_line(&h)?;
        h.rewind()?;
        let all = get_line(&h)?;
        Ok(format!("{t}|{all}"))
    })();
    check(&d, r("truncate", t1, |t| q(&t)));
    let t2 = (|| {
        let h = open(&f, FsMode::Write)?;
        put(&h, "hello")?;
        h.truncate()?;
        put(&h, "!")?;
        h.flush()?;
        Ok(fs::read_to_string(&f).unwrap())
    })();
    check(&d, r("truncate write", t2, |t| q(&t)));
}

/// `lock`, `tryLock` and `unlock` in the probe's order, and a lock released
/// when the handle's last clone drops (Lean closes a handle after its last
/// use). In a child of its own: a process another test spawns copies every
/// descriptor until its `exec`, which keeps a closed handle's lock alive.
#[test]
fn io_handle_lock() {
    if let Some(f) = child_case() {
        let lines = vec![
            r(
                "lock",
                (|| {
                    let h1 = open(&f, FsMode::Read)?;
                    let h2 = open(&f, FsMode::Read)?;
                    h1.lock(true)?;
                    let a = h2.try_lock(true)?;
                    let b = h2.try_lock(false)?;
                    h1.unlock()?;
                    let c = h2.try_lock(true)?;
                    h2.unlock()?;
                    h1.lock(false)?;
                    // Lean closes `h1` after its last use, releasing its lock
                    drop(h1);
                    let e = h2.try_lock(false)?;
                    let g = h2.try_lock(true)?;
                    Ok(vec![a, b, c, e, g])
                })(),
                |v| format!("{v:?}"),
            ),
            r(
                "lock released at last use",
                (|| {
                    let h1 = open(&f, FsMode::Read)?;
                    h1.lock(true)?;
                    let held = h1.clone();
                    drop(h1);
                    let h2 = open(&f, FsMode::Read)?;
                    // a clone still holds the lock
                    let blocked = !h2.try_lock(true)?;
                    drop(held);
                    Ok(blocked && h2.try_lock(true)?)
                })(),
                |b| b.to_string(),
            ),
        ];
        report(&lines);
    }
    let d = setup("lock");
    let f = p(&d, "h.txt");
    fs::write(&f, "x").unwrap();
    let out = child("io_handle_lock", &f, |_| {});
    let got = results(&out);
    assert_eq!(
        got.len(),
        2,
        "child stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(got[0], lean("lock", &d));
    assert_eq!(got[1], lean("lock released at last use", &d));
}

#[test]
fn io_handle_try_lock_unlock() {
    let d = setup("try_lock");
    let f = p(&d, "h.txt");
    fs::write(&f, "x").unwrap();
    let h1 = open(&f, FsMode::Read).unwrap();
    let h2 = open(&f, FsMode::Read).unwrap();
    // two shared locks coexist; an exclusive one would block (`EWOULDBLOCK` is `false`)
    assert!(h1.try_lock(false).unwrap());
    assert!(h2.try_lock(false).unwrap());
    assert!(!h1.try_lock(true).unwrap());
    h2.unlock().unwrap();
    assert!(h1.try_lock(true).unwrap());
    h1.unlock().unwrap();
    h1.unlock().unwrap();
}

#[test]
fn io_handle_is_tty() {
    let d = setup("is_tty");
    let f = p(&d, "h.txt");
    fs::write(&f, "x").unwrap();
    check(
        &d,
        r(
            "isTty file",
            open(&f, FsMode::Read).map(|h| h.is_tty()),
            |b| b.to_string(),
        ),
    );
    assert_eq!(Handle::stdout().is_tty(), std::io::stdout().is_terminal());
    assert_eq!(Handle::stdin().is_tty(), std::io::stdin().is_terminal());
}

/// A file handle is flushed when its last clone drops; `exit` flushes the
/// open handles and gives the status modulo 256 (probe `ret`: `main`
/// returning 300 exits 44 with the file's data written).
#[test]
fn io_exit_flushes_handles() {
    if child_case().as_deref() == Some("exit300") {
        let f = std::env::var("IO_TEST_FILE").unwrap();
        let h = open(&f, FsMode::Write).unwrap();
        put(&h, "file data").unwrap();
        exit::exit(300);
    }
    let d = setup("flush_handles");
    let f = p(&d, "h.txt");
    let h = open(&f, FsMode::Write).unwrap();
    put(&h, "abc").unwrap();
    let h2 = h.clone();
    drop(h);
    assert_eq!(fs::read(&f).unwrap(), b"");
    put(&h2, "d").unwrap();
    drop(h2);
    assert_eq!(fs::read(&f).unwrap(), b"abcd");
    let out = child("io_exit_flushes_handles", "exit300", |c| {
        c.env("IO_TEST_FILE", p(&d, "exit.txt"));
    });
    assert_eq!(out.status.code(), Some(44));
    assert_eq!(fs::read_to_string(p(&d, "exit.txt")).unwrap(), "file data");
}

// ---- the block policy (glibc's buffer sizes and direct writes) ----

/// In a child: the writes and flushes of `ops` (`w<n>` writes `n` bytes, `f`
/// flushes) on `h`, then `IO.Process.forceExit`.
fn run_ops(h: &Handle, ops: &str) -> ! {
    for op in ops.split(' ') {
        if op == "f" {
            h.flush().unwrap();
        } else {
            let n: usize = op[1..].parse().unwrap();
            h.write(&vec![0xFE; n]).unwrap();
        }
    }
    exit::force_exit(0)
}

/// The bytes visible after `forceExit` for each probe case of `buf.txt`:
/// standard output to a pipe and to a file, and a file handle.
#[test]
fn io_block_policy() {
    if let Some(case) = child_case() {
        let (target, ops) = case.split_once('|').unwrap();
        let h = match target {
            "stdout" => Handle::stdout(),
            path => open(path, FsMode::Write).unwrap(),
        };
        run_ops(&h, ops);
    }
    let d = setup("block");
    let count = |b: &[u8]| b.iter().filter(|&&c| c == 0xFE).count();
    for line in data("buf.txt").lines() {
        let (ops, rest) = line.split_once(": ").unwrap();
        let want: Vec<usize> = rest
            .split(' ')
            .map(|kv| kv.split_once('=').unwrap().1.parse().unwrap())
            .collect();
        let pipe = child("io_block_policy", &format!("stdout|{ops}"), |_| {});
        let out_file = p(&d, "out.txt");
        let file = child("io_block_policy", &format!("stdout|{ops}"), |c| {
            c.stdout(fs::File::create(&out_file).unwrap());
        });
        assert_eq!(file.status.code(), Some(0));
        let handle_file = p(&d, "h.txt");
        let _ = fs::remove_file(&handle_file);
        child("io_block_policy", &format!("{handle_file}|{ops}"), |_| {});
        let got = vec![
            count(&pipe.stdout),
            count(&fs::read(&out_file).unwrap()),
            fs::read(&handle_file).unwrap().len(),
        ];
        assert_eq!(got, want, "{ops}: pipe, stdout to a file, file handle");
    }
}

/// Writes after a read, a rewind or a flush on a `readWrite` handle
/// (`bufrw.txt`): the bytes on disk after `forceExit`.
#[test]
fn io_block_policy_read_write() {
    if let Some(case) = child_case() {
        let (path, ops) = case.split_once('|').unwrap();
        let h = open(path, FsMode::ReadWrite).unwrap();
        for op in ops.split(' ') {
            match op.as_bytes()[0] {
                b'f' => h.flush().unwrap(),
                b'R' => h.rewind().unwrap(),
                b'r' => drop(read(&h, op[1..].parse().unwrap()).unwrap()),
                _ => h.write(&vec![b'x'; op[1..].parse().unwrap()]).unwrap(),
            }
        }
        exit::force_exit(0);
    }
    let d = setup("block_rw");
    let f = p(&d, "rw.txt");
    for line in data("bufrw.txt").lines() {
        let (ops, want) = line.split_once(": x=").unwrap();
        fs::write(&f, vec![b'a'; 20000]).unwrap();
        let out = child("io_block_policy_read_write", &format!("{f}|{ops}"), |_| {});
        assert_eq!(out.status.code(), Some(0));
        let x = fs::read(&f).unwrap().iter().filter(|&&c| c == b'x').count();
        assert_eq!(x.to_string(), want, "{ops}");
    }
}

// ---- standard streams ----

/// Lean's probe `stdin`: `getLine`, `read 3`, then three more `getLine`s on
/// standard input.
#[test]
fn io_stdin_handle() {
    if child_case().as_deref() == Some("stdin") {
        let h = Handle::stdin();
        let a = get_line(&h).unwrap();
        let b = read(&h, 3).unwrap();
        let mut all = vec![a, b.len().to_string()];
        all.extend((0..3).map(|_| get_line(&h).unwrap()));
        report(&[quoted_list(&all)]);
    }
    // lean (probe stdin, input "first\nab"): ["first\n", "2", "", "", ""]
    let out = child_with_stdin("io_stdin_handle", "stdin", b"first\nab");
    assert_eq!(results(&out), [r#"["first\n", "2", "", "", ""]"#]);
    // lean (probe stdin, input "first\nabcdef\nlast"): ["first\n", "3", "def\n", "last", ""]
    let out = child_with_stdin("io_stdin_handle", "stdin", b"first\nabcdef\nlast");
    assert_eq!(results(&out), [r#"["first\n", "3", "def\n", "last", ""]"#]);
}

#[test]
fn io_stdout_stderr_handles() {
    if child_case().as_deref() == Some("stdout") {
        put(&Handle::stdout(), "\nRESULT: one\n").unwrap();
        put(&Handle::stdout(), "RESULT: two\n").unwrap();
        exit::exit(0);
    }
    if child_case().as_deref() == Some("stderr") {
        put(&Handle::stderr(), "to stderr\n").unwrap();
        exit::force_exit(0);
    }
    let out = child("io_stdout_stderr_handles", "stdout", |_| {});
    assert_eq!(results(&out), ["one", "two"]);
    // unbuffered: the text is out before `forceExit`
    let out = child("io_stdout_stderr_handles", "stderr", |_| {});
    assert_eq!(String::from_utf8_lossy(&out.stderr), "to stderr\n");
    let ebadf = r#"InvalidArgument(None, 9, "bad file descriptor")"#;
    assert_eq!(desc(&get_line(&Handle::stderr()).unwrap_err()), ebadf);
}

// ---- paths ----

#[test]
fn io_create_dir() {
    let d = setup("create_dir");
    check(&d, r("createDir tmp", lfs::create_dir(s("/tmp")), unit));
    check(
        &d,
        r(
            "createDir missing parent",
            lfs::create_dir(s("/nonexistent/x/y")),
            unit,
        ),
    );
    check(&d, r("createDir nul", lfs::create_dir(s("a\0b")), unit));
    check(
        &d,
        r(
            "createDir file parent",
            lfs::create_dir(p(&d, "full/f/x").as_bytes()),
            unit,
        ),
    );
    let made = lfs::create_dir(p(&d, "newdir").as_bytes())
        .and_then(|()| lfs::metadata(p(&d, "newdir").as_bytes()));
    check(&d, r("createDir ok", made, show_type));
}

#[test]
fn io_remove_file() {
    let d = setup("remove_file");
    check(
        &d,
        r(
            "removeFile missing",
            lfs::remove_file(p(&d, "missing").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "removeFile dir",
            lfs::remove_file(p(&d, "full").as_bytes()),
            unit,
        ),
    );
    lfs::remove_file(p(&d, "full/f").as_bytes()).unwrap();
    assert!(!std::path::Path::new(&p(&d, "full/f")).exists());
}

#[test]
fn io_remove_dir() {
    let d = setup("remove_dir");
    check(
        &d,
        r(
            "removeDir missing",
            lfs::remove_dir(p(&d, "missing").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "removeDir full",
            lfs::remove_dir(p(&d, "full").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "removeDir notdir",
            lfs::remove_dir(p(&d, "full/f").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "removeDir file",
            lfs::remove_dir(p(&d, "bytes.txt").as_bytes()),
            unit,
        ),
    );
    fs::create_dir(p(&d, "empty")).unwrap();
    lfs::remove_dir(p(&d, "empty").as_bytes()).unwrap();
}

#[test]
fn io_rename() {
    let d = setup("rename");
    check(
        &d,
        r(
            "rename missing",
            lfs::rename(p(&d, "missing").as_bytes(), p(&d, "other").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "rename nul",
            lfs::rename(s("a\0b"), p(&d, "other").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "rename nul2",
            lfs::rename(p(&d, "full/f").as_bytes(), s("a\0b")),
            unit,
        ),
    );
    check(
        &d,
        r(
            "rename dir onto file",
            lfs::rename(p(&d, "full").as_bytes(), p(&d, "full/f").as_bytes()),
            unit,
        ),
    );
    fs::write(p(&d, "r1"), "moved").unwrap();
    let moved = lfs::rename(p(&d, "r1").as_bytes(), p(&d, "r2").as_bytes())
        .map(|()| fs::read_to_string(p(&d, "r2")).unwrap());
    check(&d, r("rename ok", moved, |t| q(&t)));
}

#[test]
fn io_hard_link() {
    let d = setup("hard_link");
    check(
        &d,
        r(
            "hardLink missing",
            lfs::hard_link(p(&d, "missing").as_bytes(), p(&d, "l").as_bytes()),
            unit,
        ),
    );
    check(
        &d,
        r(
            "hardLink exists",
            lfs::hard_link(p(&d, "full/f").as_bytes(), p(&d, "full/f").as_bytes()),
            unit,
        ),
    );
    let linked = lfs::hard_link(p(&d, "full/f").as_bytes(), p(&d, "full/f2").as_bytes())
        .and_then(|()| lfs::metadata(p(&d, "full/f").as_bytes()));
    check(
        &d,
        r("hardLink ok", linked, |m| {
            format!("links={} size={}", m.num_links, m.byte_size)
        }),
    );
}

fn real_path(path: &[u8]) -> Result<String, IoError> {
    let mut v = Vec::new();
    lfs::real_path(path, &mut v)?;
    Ok(String::from_utf8(v).unwrap())
}

#[test]
fn io_real_path() {
    let d = setup("real_path");
    check(
        &d,
        r(
            "realPath missing",
            real_path(p(&d, "missing").as_bytes()),
            |t| t,
        ),
    );
    check(
        &d,
        r(
            "realPath ok",
            real_path(p(&d, "full/../full/f").as_bytes()),
            |t| t,
        ),
    );
    check(&d, r("realPath nul", real_path(s("a\0b")), |t| t));
    check(
        &d,
        r(
            "realPath notdir",
            real_path(p(&d, "full/f/g").as_bytes()),
            |t| t,
        ),
    );
}

/// In a child (it changes its working directory): `realPath "."` in a
/// directory whose path is 4095 bytes long resolves; one byte longer, glibc's
/// `realpath` into Lean's `PATH_MAX` buffer fails, and Lean reports
/// `mk_file_not_found_error` (io-2's case `uvsys/cwd_long`, recorded on native
/// 4.34.0, shows the same for 4096 and 4297 bytes).
#[test]
fn io_real_path_length_cap() {
    if let Some(d) = child_case() {
        lfs::set_current_dir(d.as_bytes()).unwrap();
        let mut cur = process_current_dir().unwrap().len();
        // components of 200 bytes, then a last one that ends at 4095 or 4096
        let mut k = 0;
        while 4095 - cur > 250 {
            let c = format!("{k:03}{}", "d".repeat(197));
            lfs::create_dir(c.as_bytes()).unwrap();
            lfs::set_current_dir(c.as_bytes()).unwrap();
            cur += 1 + c.len();
            k += 1;
        }
        let mut lines = Vec::new();
        for len in [4095, 4096] {
            let c = "e".repeat(len - cur - 1);
            lfs::create_dir(c.as_bytes()).unwrap();
            lfs::set_current_dir(c.as_bytes()).unwrap();
            let cwd = process_current_dir().map(|p| p.len());
            lines.push(r(&format!("{len}"), real_path(b"."), |t| {
                format!("{} bytes, cwd {cwd:?}", t.len())
            }));
            lfs::set_current_dir(b"..").unwrap();
        }
        report(&lines);
    }
    let d = setup("real_path_cap");
    let out = child("io_real_path_length_cap", &d, |_| {});
    assert_eq!(
        results(&out),
        [
            "4095: ok 4095 bytes, cwd Ok(4095)".to_owned(),
            r#"4096: err NoFileOrDirectory(".", 2, "")"#.to_owned(),
        ],
        "child stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every `realpath` failure is `mk_file_not_found_error`, code 2 and no
/// details, whatever the `errno`: `ELOOP`, `EACCES` (unless the tests run as
/// root), `ENOTDIR`, `ENOENT`; the NUL check comes first.
#[test]
fn io_real_path_errors_are_file_not_found() {
    let d = setup("real_path_errors");
    let nf = |p: &str| IoError::NoFileOrDirectory(p.to_owned(), 2, String::new());
    std::os::unix::fs::symlink("b", p(&d, "a")).unwrap();
    std::os::unix::fs::symlink("a", p(&d, "b")).unwrap();
    assert_eq!(real_path(p(&d, "a").as_bytes()), Err(nf(&p(&d, "a"))));
    assert_eq!(
        real_path(p(&d, "full/f/g").as_bytes()),
        Err(nf(&p(&d, "full/f/g")))
    );
    assert_eq!(
        real_path(p(&d, "missing").as_bytes()),
        Err(nf(&p(&d, "missing")))
    );
    assert_eq!(
        real_path(b"a\0/missing"),
        Err(IoError::embedded_nul(b"a\0/missing"))
    );
    fs::create_dir(p(&d, "locked")).unwrap();
    fs::write(p(&d, "locked/x"), "x").unwrap();
    fs::set_permissions(p(&d, "locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let denied = fs::metadata(p(&d, "locked/x"))
        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
    let got = real_path(p(&d, "locked/x").as_bytes());
    fs::set_permissions(p(&d, "locked"), fs::Permissions::from_mode(0o755)).unwrap();
    if denied {
        assert_eq!(got, Err(nf(&p(&d, "locked/x"))));
    }
}

#[test]
fn io_metadata() {
    let d = setup("metadata");
    check(
        &d,
        r(
            "metadata missing",
            lfs::metadata(p(&d, "missing").as_bytes()),
            |_| "m".into(),
        ),
    );
    check(
        &d,
        r(
            "metadata notdir",
            lfs::metadata(p(&d, "full/f/g").as_bytes()),
            |_| "m".into(),
        ),
    );
    check(
        &d,
        r("metadata nul", lfs::metadata(s("a\0b")), |_| "m".into()),
    );
    let file = lfs::metadata(p(&d, "full/f").as_bytes());
    check(
        &d,
        r("metadata file", file, |m| {
            format!(
                "size={} type={} nsec<1e9={}",
                m.byte_size,
                file_type(m.file_type),
                m.modified.nsec < 1_000_000_000
            )
        }),
    );
    check(
        &d,
        r(
            "metadata dir",
            lfs::metadata(p(&d, "full").as_bytes()),
            show_type,
        ),
    );
    check(
        &d,
        r(
            "metadata link",
            lfs::metadata(p(&d, "link").as_bytes()),
            show_type,
        ),
    );
    check(
        &d,
        r(
            "metadata dangling",
            lfs::metadata(p(&d, "dangling").as_bytes()),
            show_type,
        ),
    );
    check(
        &d,
        r("metadata other", lfs::metadata(s("/dev/null")), show_type),
    );
    // the times are the file's: `stat`'s seconds and nanoseconds
    let m = lfs::metadata(p(&d, "full/f").as_bytes()).unwrap();
    let mtime = fs::metadata(p(&d, "full/f"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(m.modified.sec, mtime.as_secs() as i64);
    assert_eq!(m.modified.nsec, mtime.subsec_nanos());
    assert_eq!(m.num_links, 1);
}

#[test]
fn io_symlink_metadata() {
    let d = setup("symlink_metadata");
    check(
        &d,
        r(
            "symlinkMetadata missing",
            lfs::symlink_metadata(p(&d, "missing").as_bytes()),
            |_| "m".into(),
        ),
    );
    check(
        &d,
        r(
            "symlinkMetadata link",
            lfs::symlink_metadata(p(&d, "link").as_bytes()),
            show_type,
        ),
    );
    check(
        &d,
        r(
            "symlinkMetadata dangling",
            lfs::symlink_metadata(p(&d, "dangling").as_bytes()),
            show_type,
        ),
    );
}

fn read_dir(path: &str) -> Result<Vec<Vec<u8>>, IoError> {
    let mut names = Vec::new();
    lfs::read_dir(path.as_bytes(), |n| names.push(n.to_vec()))?;
    Ok(names)
}

#[test]
fn io_read_dir() {
    let d = setup("read_dir");
    check(
        &d,
        r("readDir missing", read_dir(&p(&d, "missing")), |_| {
            "a".into()
        }),
    );
    check(
        &d,
        r("readDir file", read_dir(&p(&d, "full/f")), |_| "a".into()),
    );
    let full = read_dir(&p(&d, "full")).map(|v| {
        let items: Vec<String> = v
            .iter()
            .map(|n| format!("({}, {})", p(&d, "full"), String::from_utf8_lossy(n)))
            .collect();
        format!("[{}]", items.join(", "))
    });
    check(&d, r("readDir ok", full, |t| t));
    // lean (probe fs, `readDir lossy`): the two names, in the file system's
    // order, before Lean's lossy decoding
    let mut names = read_dir(&p(&d, "names")).unwrap();
    names.sort();
    assert_eq!(names, [&b"a\xffb"[..], b"\xc0\x80"]);
}

#[test]
fn io_set_access_rights() {
    let d = setup("set_access_rights");
    check(
        &d,
        r(
            "chmod missing",
            lfs::set_access_rights(p(&d, "missing").as_bytes(), 0o644),
            unit,
        ),
    );
    lfs::set_access_rights(p(&d, "full/f").as_bytes(), 0o600).unwrap();
    let mode = fs::metadata(p(&d, "full/f")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

fn current_dir() -> Result<String, IoError> {
    let mut v = Vec::new();
    lfs::current_dir(&mut v)?;
    Ok(String::from_utf8(v).unwrap())
}

fn process_current_dir() -> Result<String, IoError> {
    let mut v = Vec::new();
    lfs::process_current_dir(&mut v)?;
    Ok(String::from_utf8(v).unwrap())
}

/// The working directory changes in a child, so the other tests keep theirs.
#[test]
fn io_set_current_dir() {
    if let Some(d) = child_case() {
        let lines = vec![
            r(
                "setCurrentDir missing",
                lfs::set_current_dir(p(&d, "missing").as_bytes()),
                unit,
            ),
            r(
                "setCurrentDir file",
                lfs::set_current_dir(p(&d, "full/f").as_bytes()),
                unit,
            ),
            r(
                "ok",
                lfs::set_current_dir(p(&d, "full").as_bytes()).and_then(|()| current_dir()),
                |t| t,
            ),
            // Lean passes the C string: the part before a NUL byte
            r(
                "nul",
                lfs::set_current_dir(format!("{d}\0/full").as_bytes()).and_then(|()| current_dir()),
                |t| t,
            ),
        ];
        report(&lines);
    }
    let d = setup("set_current_dir");
    let out = child("io_set_current_dir", &d, |_| {});
    let got = results(&out);
    assert_eq!(got[0], lean("setCurrentDir missing", &d));
    assert_eq!(got[1], lean("setCurrentDir file", &d));
    assert_eq!(got[2], format!("ok: ok {d}/full"));
    assert_eq!(got[3], format!("nul: ok {d}"));
}

/// Lean's probe `cwd`: `IO.currentDir` and `IO.Process.getCurrentDir` in a
/// directory, and after it is removed: `IO.currentDir` fails with its fixed
/// text; `getCurrentDir` crashes compiled Lean (LB-03) and is the error
/// without a file name here.
#[test]
fn io_current_dir() {
    if let Some(d) = child_case() {
        let cwd = p(&d, "cwd");
        lfs::create_dir(cwd.as_bytes()).unwrap();
        lfs::set_current_dir(cwd.as_bytes()).unwrap();
        let mut lines = vec![
            r("currentDir ok", current_dir(), |t| t),
            r("getCurrentDir ok", process_current_dir(), |t| t),
        ];
        lfs::remove_dir(cwd.as_bytes()).unwrap();
        lines.push(r("currentDir deleted", current_dir(), |t| t));
        lines.push(r("getCurrentDir deleted", process_current_dir(), |t| t));
        report(&lines);
    }
    let d = setup("current_dir");
    let out = child("io_current_dir", &d, |_| {});
    // lean (probe cwd): currentDir ok: ok <d>/cwd; getCurrentDir ok: ok <d>/cwd;
    // currentDir deleted: err UserError("failed to retrieve current working directory");
    // getCurrentDir deleted: SIGSEGV (LB-03)
    assert_eq!(
        results(&out),
        [
            format!("currentDir ok: ok {d}/cwd"),
            format!("getCurrentDir ok: ok {d}/cwd"),
            r#"currentDir deleted: err UserError("failed to retrieve current working directory")"#
                .to_owned(),
            r#"getCurrentDir deleted: err NoFileOrDirectory("", 2, "no such file or directory")"#
                .to_owned(),
        ]
    );
}

#[test]
fn io_get_current_dir() {
    let here = std::env::current_dir().unwrap();
    assert_eq!(process_current_dir().unwrap(), here.to_str().unwrap());
    assert_eq!(current_dir().unwrap(), here.to_str().unwrap());
}

// ---- process ----

#[test]
fn io_app_path() {
    let exe = std::env::current_exe().unwrap();
    let mut v = Vec::new();
    env::app_path(&mut v).unwrap();
    assert_eq!(v, exe.as_os_str().as_bytes());
}

/// Lean's probe `env`: names with a NUL byte or an `=` and empty names are
/// absent; the value's bytes are the translator's to decode (lossily).
#[test]
fn io_get_env() {
    if child_case().as_deref() == Some("env") {
        let names = [
            "LEANRS_PROBE_A",
            "LEANRS_PROBE_B",
            "",
            "LEANRS\0PROBE_A",
            "LEANRS_PROBE_A=x",
            "LEANRS_PROBE_NONE",
        ];
        let lines: Vec<String> = names
            .iter()
            .map(|n| {
                let mut v = Vec::new();
                let shown = if env::get_env(n.as_bytes(), &mut v) {
                    format!("some {v:?}")
                } else {
                    "none".to_owned()
                };
                format!("{}: {shown}", q(n))
            })
            .collect();
        report(&lines);
    }
    let out = child("io_get_env", "env", |c| {
        c.env("LEANRS_PROBE_A", OsStr::from_bytes(b"v\xc0\x80"))
            .env("LEANRS_PROBE_B", "")
            .env_remove("LEANRS_PROBE_NONE");
    });
    // lean (probe env, LEANRS_PROBE_A=$'v\xc0\x80' LEANRS_PROBE_B=): A is
    // (some "v\u{fffd}") after Lean's lossy decoding of the bytes, B is (some ""),
    // the other four none
    assert_eq!(
        results(&out),
        [
            r#""LEANRS_PROBE_A": some [118, 192, 128]"#,
            r#""LEANRS_PROBE_B": some []"#,
            r#""": none"#,
            r#""LEANRS\x00PROBE_A": none"#,
            r#""LEANRS_PROBE_A=x": none"#,
            r#""LEANRS_PROBE_NONE": none"#,
        ]
    );
}

/// glibc's `getenv` with a name holding `=` matches the start of an entry:
/// `A=x` finds the entry `A=x=v` and gives `v`.
#[test]
fn io_get_env_eq() {
    if child_case().as_deref() == Some("eq") {
        let mut v = Vec::new();
        let found = env::get_env(b"LEAN_RUNTIME_EQ=x", &mut v);
        report(&[if found {
            format!("some {v:?}")
        } else {
            "none".to_owned()
        }]);
    }
    let out = child("io_get_env_eq", "eq", |c| {
        c.env("LEAN_RUNTIME_EQ", "x=v");
    });
    assert_eq!(results(&out), [r#"some [118]"#]);
}

#[test]
fn io_mono_clock() {
    let a = env::mono_nanos_now();
    let ms = env::mono_ms_now();
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(5));
    let b = env::mono_nanos_now();
    let elapsed = start.elapsed().as_nanos() as u64;
    assert!(b >= a + 5_000_000);
    assert!(b - a <= elapsed + 1_000_000_000);
    assert!(env::mono_ms_now() >= ms + 5);
    // CLOCK_MONOTONIC counts from boot (DV10: Lean's origin) without the time
    // spent suspended, which /proc/uptime (CLOCK_BOOTTIME) counts, so it
    // never exceeds the uptime read after it (review RIO1-07)
    let mono = env::mono_nanos_now() as f64 / 1e9;
    let up: f64 = fs::read_to_string("/proc/uptime")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        mono > 0.0 && mono <= up + 0.5,
        "monotonic {mono} s, uptime {up} s"
    );
}

/// `end_startup` (and its old name `fail_as_native`) ends the process in
/// order where native crashes (LB-30: SIGSEGV after a failed loop; LB-31:
/// SIGABRT at the signal lock): `INTERNAL PANIC: Failed to initialize event
/// loop: <libuv's message>`, status 1, nothing on stdout; under
/// `LEAN_ABORT_ON_PANIC`, the line then an abort (134). In children, without
/// core dumps.
#[test]
fn io_startup_end() {
    use lean_runtime::io::startup::{end_startup, fail_as_native, StartupFailure};
    use std::os::unix::process::ExitStatusExt;
    match child_case().as_deref() {
        Some("loop") | Some("loop-abort") => end_startup(StartupFailure::LoopInit(24)),
        Some("lock") => fail_as_native(StartupFailure::SignalLock(23)),
        _ => {}
    }
    for (case, abort, code, signal, msg) in [
        ("loop", false, Some(1), None, "too many open files"),
        ("lock", false, Some(1), None, "file table overflow"),
        ("loop-abort", true, None, Some(6), "too many open files"),
    ] {
        let exe = std::env::current_exe().expect("test binary path");
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "ulimit -c 0; exec \"$0\" io_startup_end --exact --nocapture --test-threads=1",
        ])
        .arg(&exe)
        .env(CHILD_VAR, case)
        .env_remove("LEAN_ABORT_ON_PANIC")
        .stdin(Stdio::null());
        if abort {
            cmd.env("LEAN_ABORT_ON_PANIC", "1");
        }
        let out = cmd.output().expect("child");
        assert_eq!(out.status.code(), code, "{case}: {:?}", out.status);
        assert_eq!(out.status.signal(), signal, "{case}: {:?}", out.status);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.ends_with(&format!(
                "INTERNAL PANIC: Failed to initialize event loop: {msg}\n"
            )),
            "{case}: {err:?}"
        );
    }
}

fn random(n: usize) -> Result<Vec<u8>, IoError> {
    env::check_random_size(n)?;
    let mut v = vec![0u8; n];
    env::get_random_bytes(&mut v)?;
    Ok(v)
}

#[test]
fn io_get_random_bytes() {
    let d = setup("random");
    check(&d, r("random 0", random(0), |b| b.len().to_string()));
    check(&d, r("random 33", random(33), |b| b.len().to_string()));
    assert_ne!(random(32).unwrap(), random(32).unwrap());
    assert_eq!(
        desc(&random(usize::MAX).unwrap_err()),
        r#"ResourceExhausted(None, 12, "not enough memory")"#
    );
}

/// `env::open_random` then `RandomSource::fill` (AR-1): the same results as
/// `check_random_size` and `get_random_bytes`, the open first.
fn random_opened_first(n: usize) -> Result<Vec<u8>, IoError> {
    let src = env::open_random(n)?;
    let mut v = vec![0u8; n];
    src.fill(&mut v)?;
    Ok(v)
}

#[test]
fn io_open_random() {
    let d = setup("random-first");
    check(
        &d,
        r("random 0", random_opened_first(0), |b| b.len().to_string()),
    );
    check(
        &d,
        r("random 33", random_opened_first(33), |b| {
            b.len().to_string()
        }),
    );
    assert_ne!(
        random_opened_first(32).unwrap(),
        random_opened_first(32).unwrap()
    );
    assert_eq!(
        desc(&env::open_random(usize::MAX).unwrap_err()),
        r#"ResourceExhausted(None, 12, "not enough memory")"#
    );
    // a source opened for 0 bytes and given more opens `/dev/urandom` then
    let mut v = vec![0u8; 64];
    env::open_random(0).unwrap().fill(&mut v).unwrap();
    assert!(v.iter().any(|&b| b != 0));
    use std::mem::MaybeUninit;
    let mut u = vec![MaybeUninit::new(0u8); 4096];
    env::open_random(u.len())
        .unwrap()
        .fill_uninit(&mut u)
        .unwrap();
    // SAFETY: every element was initialized above, and again by the call.
    let bytes: Vec<u8> = u.iter().map(|b| unsafe { b.assume_init() }).collect();
    assert!(
        bytes.iter().filter(|&&b| b == 0).count() < 64,
        "the bytes look unwritten"
    );
}

/// The modelled `errno` as native's `getLine` reported it on a stream whose
/// error indicator an earlier failure had set (`decode_io_error(errno,
/// nullptr)`). Since LB-41 `getLine` reports only its own error, so the
/// tests of the model read it directly.
fn modelled_errno() -> String {
    desc(&IoError::decode_io_error(
        lean_runtime::io::error::errno(),
        None,
    ))
}

/// The modelled `errno`: the code C's `errno` holds after an operation,
/// which natively a `getLine` whose check finds the error indicator set
/// reported (until LB-41). Later operations change it: every failure, and
/// four successes (`realPath`'s `EINVAL` walk, a `false` `isTty`, a `false`
/// `tryLock`, a flush of read-ahead that cannot seek back); the libuv-based
/// rows clear it; a task's is its own. From leanrs's `io_modelled_errno`
/// (`local/follow-native`; compiled Lean v4.34.0-rc1, leanrs's A820; leanrs
/// review F2).
#[test]
fn io_modelled_errno() {
    let d = setup("errno");
    let k = std::cell::Cell::new(0);
    let code_after = |act: &dyn Fn()| -> String {
        k.set(k.get() + 1);
        let f = p(&d, &format!("e{}.txt", k.get()));
        fs::write(&f, "line1\n").unwrap();
        let h = open(&f, FsMode::Read).unwrap();
        assert!(put(&h, "x").is_err());
        act();
        let code = modelled_errno();
        // LB-41: the indicator the failed write set fails no later `getLine`
        assert_eq!(get_line(&h).unwrap(), "line1\n");
        code
    };
    let ebadf = r#"InvalidArgument(None, 9, "bad file descriptor")"#;
    let einval = r#"InvalidArgument(None, 22, "invalid argument")"#;
    assert_eq!(code_after(&|| ()), ebadf);
    assert_eq!(
        code_after(&|| drop(real_path(b"/usr/bin").unwrap())),
        einval
    );
    assert_eq!(code_after(&|| drop(real_path(b"/usr/..").unwrap())), einval);
    assert_eq!(code_after(&|| drop(real_path(b"/").unwrap())), ebadf);
    assert_eq!(code_after(&|| drop(real_path(b".").unwrap())), ebadf);
    let probe = p(&d, "probe.txt");
    fs::write(&probe, "x").unwrap();
    assert_eq!(
        code_after(&|| assert!(!open(&probe, FsMode::Read).unwrap().is_tty())),
        r#"IllegalOperation(25, "inappropriate ioctl for device")"#
    );
    let lock = p(&d, "lock.txt");
    let a = open(&lock, FsMode::Write).unwrap();
    a.lock(true).unwrap();
    assert_eq!(
        code_after(&|| assert!(!open(&lock, FsMode::Write).unwrap().try_lock(true).unwrap())),
        r#"ResourceExhausted(None, 11, "resource temporarily unavailable")"#
    );
    drop(a);
    assert_eq!(
        code_after(&|| drop(lfs::create_dir(b".").unwrap_err())),
        r#"AlreadyExists(None, 17, "file already exists")"#
    );
    assert_eq!(code_after(&|| drop(read_dir(".").unwrap())), ebadf);
    // the rows Lean runs through libuv's `uv_fs_*` clear it to 0
    let zero = r#"OtherError(0, "Unknown system error 0")"#;
    assert_eq!(
        code_after(&|| {
            lfs::metadata(probe.as_bytes()).unwrap();
        }),
        zero
    );
    assert_eq!(
        code_after(&|| {
            lfs::symlink_metadata(probe.as_bytes()).unwrap();
        }),
        zero
    );
    let rm = p(&d, "rm.txt");
    assert_eq!(
        code_after(&|| {
            fs::write(&rm, "x").unwrap();
            lfs::remove_file(rm.as_bytes()).unwrap();
        }),
        zero
    );
    let link = p(&d, "link.txt");
    assert_eq!(
        code_after(&|| lfs::hard_link(probe.as_bytes(), link.as_bytes()).unwrap()),
        zero
    );
    // a task's (a thread's) `errno` is its own
    assert_eq!(
        code_after(&|| {
            std::thread::spawn(|| drop(real_path(b"/usr/bin").unwrap()))
                .join()
                .unwrap();
        }),
        ebadf
    );
}

/// A flush of standard input from a pipe gives the read-ahead back without
/// the seek, which fails with `ESPIPE`; the flush succeeds and C's `errno`
/// keeps `ESPIPE` (compiled Lean v4.34.0-rc1: the report of a `getLine` on a
/// stream whose error indicator was set is `unsupported operation (error
/// code: 29, invalid seek)`; since LB-41 the test reads the model directly).
/// From leanrs's `io_modelled_errno_stdin_flush` (leanrs review F2).
#[test]
fn io_modelled_errno_stdin_flush() {
    if child_case().as_deref() == Some("pipe") {
        let f = std::env::var("IO_TEST_FILE").unwrap();
        let h = open(&f, FsMode::Read).unwrap();
        assert!(put(&h, "x").is_err());
        let i = Handle::stdin();
        get_line(&i).unwrap();
        i.flush().unwrap();
        report(&[modelled_errno()]);
    }
    let d = setup("errno-stdin");
    let f = p(&d, "r.txt");
    fs::write(&f, "line\n").unwrap();
    let mut c = Command::new(std::env::current_exe().expect("test binary path"));
    c.args([
        "io_modelled_errno_stdin_flush",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD_VAR, "pipe")
    .env("IO_TEST_FILE", &f);
    let out = {
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut proc = c.spawn().expect("child test process");
        proc.stdin
            .take()
            .expect("stdin pipe")
            .write_all(b"abc\ndef\n")
            .unwrap();
        proc.wait_with_output().expect("child output")
    };
    assert_eq!(
        results(&out),
        [r#"UnsupportedOperation(29, "invalid seek")"#]
    );
}

/// `read n` into a `Vec` reserves nothing for the `n` bytes and touches only
/// the bytes read, so a huge `n` on a small file costs no memory (compiled
/// Lean v4.34.0-rc1: `read (2^40)` of a 3-byte file gives the 3 bytes,
/// leanrs's A828; leanrs review F4: the zeroing it replaced was OOM-killed).
/// From leanrs's `io_read_huge_touches_nothing`. In a child under a 4 GiB
/// address-space limit (`ulimit -v`), so reserving or zeroing 64 GiB fails
/// (review RIO1-18).
#[test]
fn io_read_huge_touches_nothing() {
    if child_case().as_deref() == Some("huge") {
        let f = std::env::var("IO_TEST_FILE").unwrap();
        let h = open(&f, FsMode::Read).unwrap();
        let mut v = Vec::new();
        let got = h.read_vec(1 << 36, &mut v);
        report(&[format!("{got:?} {v:?}")]);
    }
    let d = setup("read-huge");
    let f = p(&d, "abc.txt");
    fs::write(&f, "abc").unwrap();
    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new("/bin/sh")
        .args(["-c", "ulimit -v 4194304; exec \"$0\" io_read_huge_touches_nothing --exact --nocapture --test-threads=1"])
        .arg(&exe)
        .env(CHILD_VAR, "huge")
        .env("IO_TEST_FILE", &f)
        .stdin(Stdio::null())
        .output()
        .expect("child");
    assert_eq!(results(&out), ["Ok(3) [97, 98, 99]"], "{out:?}");
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(
        desc(&lean_runtime::io::handle::check_read_size(usize::MAX).unwrap_err()),
        r#"ResourceExhausted(None, 12, "not enough memory")"#
    );
    drop(h);
}

/// `read_vec` reserves the rest of the request when it reads a regular
/// file, the refill's tail included, so the `Vec` never doubles: `readBinFile`
/// of 1 MiB + 1 bytes ends with that capacity, not 2 MiB (review RIO1-14).
#[test]
fn io_read_vec_capacity() {
    let d = setup("read-vec-capacity");
    for size in [1usize << 20 | 1, 10_000, 4096, 5] {
        let f = p(&d, &format!("{size}.bin"));
        fs::write(&f, vec![7u8; size]).unwrap();
        let h = open(&f, FsMode::Read).unwrap();
        let mut v = Vec::new();
        assert_eq!(h.read_vec(size, &mut v).unwrap(), size);
        assert_eq!(v.len(), size);
        assert!(
            // std's smallest allocation of bytes is 8
            v.capacity() <= (size + 1).max(8),
            "{size} bytes: capacity {}",
            v.capacity()
        );
    }
}

/// The read(2) calls this thread has made (`/proc/thread-self/io`).
fn reads_so_far() -> u64 {
    let io = fs::read_to_string("/proc/thread-self/io").unwrap();
    io.lines()
        .find_map(|l| l.strip_prefix("syscr: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// A file that reports size 0 (procfs) is read as glibc reads it, one
/// `read(2)` of the direct part, not std's 32-byte probes, which stitch a
/// generated file from several generations (review RIO1-15):
/// `/proc/sys/kernel/random/uuid` read with `read_vec 1024` takes two
/// `read(2)` calls (the uuid, then end of file) and gives one uuid.
#[test]
fn io_read_vec_procfs() {
    let uuid = "/proc/sys/kernel/random/uuid";
    if !std::path::Path::new(uuid).exists() {
        eprintln!("io_read_vec_procfs: no {uuid}, skipped");
        return;
    }
    let h = open(uuid, FsMode::Read).unwrap();
    let a = reads_so_far();
    let b = reads_so_far();
    let measure = b - a;
    let mut v = Vec::new();
    let got = h.read_vec(1024, &mut v).unwrap();
    let c = reads_so_far();
    assert_eq!(c - b - measure, 2, "read(2) calls of read_vec 1024");
    assert_eq!(got, 37);
    let t = String::from_utf8(v).unwrap();
    let groups: Vec<usize> = t.trim_end().split('-').map(str::len).collect();
    assert_eq!(groups, [8, 4, 4, 4, 12], "{t:?}");
}

/// `get_random_bytes_uninit` fills all of its uninitialized bytes.
#[test]
fn io_get_random_bytes_uninit() {
    use std::mem::MaybeUninit;
    let mut v = vec![MaybeUninit::new(0u8); 4096];
    env::get_random_bytes_uninit(&mut v).unwrap();
    // SAFETY: every element was initialized above, and again by the call.
    let bytes: Vec<u8> = v.iter().map(|b| unsafe { b.assume_init() }).collect();
    assert!(
        bytes.iter().filter(|&&b| b == 0).count() < 64,
        "the bytes look unwritten"
    );
    env::get_random_bytes_uninit(&mut []).unwrap();
}

#[test]
fn io_get_pid() {
    assert_eq!(env::get_pid(), std::process::id());
}

#[test]
fn io_sleep() {
    let start = Instant::now();
    env::sleep(30);
    assert!(start.elapsed() >= Duration::from_millis(30));
    env::sleep(0);
}

/// Lean's probe `exit`: `IO.Process.exit 7` after printing `pending` and
/// writing a file through a held handle: status 7, both flushed.
#[test]
fn io_process_exit() {
    if child_case().as_deref() == Some("exit") {
        let f = std::env::var("IO_TEST_FILE").unwrap();
        put(&Handle::stdout(), "pending").unwrap();
        let h = open(&f, FsMode::Write).unwrap();
        put(&h, "file data").unwrap();
        let _keep = &h;
        exit::exit(7);
    }
    let d = setup("process_exit");
    let f = p(&d, "exit.txt");
    let out = child("io_process_exit", "exit", |c| {
        c.env("IO_TEST_FILE", &f);
    });
    assert_eq!(out.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&out.stdout).contains("pending"));
    assert_eq!(fs::read_to_string(&f).unwrap(), "file data");
}

/// Lean's probe `forceexit`: `IO.Process.forceExit 9` after printing
/// `pending` and writing a file: status 9, standard output lost; the file is
/// written when its handle was closed before (Lean closes it after its last
/// use) and lost when it was still held (probe `buf`).
#[test]
fn io_process_force_exit() {
    if let Some(case) = child_case() {
        let f = std::env::var("IO_TEST_FILE").unwrap();
        put(&Handle::stdout(), "pending").unwrap();
        let h = open(&f, FsMode::Write).unwrap();
        put(&h, "file data").unwrap();
        if case == "dropped" {
            drop(h);
            exit::force_exit(9);
        }
        let _keep = &h;
        exit::force_exit(9);
    }
    let d = setup("force_exit");
    for (case, file) in [("dropped", "file data"), ("held", "")] {
        let f = p(&d, &format!("{case}.txt"));
        let out = child("io_process_force_exit", case, |c| {
            c.env("IO_TEST_FILE", &f);
        });
        assert_eq!(out.status.code(), Some(9));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("pending"));
        assert_eq!(fs::read_to_string(&f).unwrap(), file, "{case}");
    }
}

thread_local! {
    /// A handle kept in a thread-local, as a translator may keep handles (a
    /// thread-local file table, a lazily initialized global).
    static HELD: std::cell::RefCell<Option<Handle>> = const { std::cell::RefCell::new(None) };
}

/// `forceExit` (`_Exit`) loses the pending output of a handle the program
/// still holds, also when the handle lives in a thread-local that
/// `std::process::exit`'s TLS destructors drop (leanrs review F3); `exit`
/// writes it.
#[test]
fn io_force_exit_drops_thread_local_handles() {
    if let Some(case) = child_case() {
        let f = std::env::var("IO_TEST_FILE").unwrap();
        let h = open(&f, FsMode::Write).unwrap();
        put(&h, "pending").unwrap();
        HELD.with(|c| *c.borrow_mut() = Some(h));
        if case == "force" {
            exit::force_exit(3);
        }
        exit::exit(4);
    }
    let d = setup("force-exit-tls");
    for (case, code, file) in [("force", 3, ""), ("exit", 4, "pending")] {
        let f = p(&d, &format!("{case}.txt"));
        let out = child("io_force_exit_drops_thread_local_handles", case, |c| {
            c.env("IO_TEST_FILE", &f);
        });
        assert_eq!(out.status.code(), Some(code), "{case}");
        assert_eq!(fs::read_to_string(&f).unwrap(), file, "{case}");
    }
}

/// Lean's probe `uncaught`: an uncaught `userError "a\x00b"` prints
/// `uncaught exception: a` (a C string stops at the NUL byte) and exits 1;
/// pending standard output is flushed first.
#[test]
fn io_uncaught() {
    if child_case().as_deref() == Some("uncaught") {
        put(&Handle::stdout(), "before").unwrap();
        exit::show_error(b"a\0b");
        exit::exit(1);
    }
    if child_case().as_deref() == Some("order") {
        let (out, err) = (Handle::stdout(), Handle::stderr());
        for (h, t) in [
            (&out, "a"),
            (&err, "b"),
            (&out, "c"),
            (&err, "d\n"),
            (&out, "e"),
        ] {
            put(h, t).unwrap();
        }
        exit::show_error(b"boom");
        exit::exit(1);
    }
    let out = child("io_uncaught", "uncaught", |_| {});
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stderr, b"uncaught exception: a\n");
    assert!(String::from_utf8_lossy(&out.stdout).contains("before"));
    // lean (probe order, `2>&1` into one file): `bd\naceuncaught exception: boom\n`:
    // standard error writes do not flush standard output, the uncaught message does
    let d = setup("uncaught");
    let path = p(&d, "both.txt");
    let both = fs::File::create(&path).unwrap();
    let out = child("io_uncaught", "order", |c| {
        c.stdout(both.try_clone().unwrap()).stderr(both);
    });
    assert_eq!(out.status.code(), Some(1));
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.ends_with("bd\naceuncaught exception: boom\n"),
        "{text:?}"
    );
}

// ---- stream state (leanrs's port audit, values from compiled Lean, A721 to A726) ----

fn ebadf_err() -> IoError {
    IoError::InvalidArgument(None, 9, "bad file descriptor".into())
}

/// A write of no bytes succeeds on every handle, standard input and a
/// read-only file included; one byte fails with `EBADF` (A721).
#[test]
fn io_zero_byte_writes() {
    let d = setup("zero-writes");
    let f = p(&d, "ro.txt");
    fs::write(&f, "line1\n").unwrap();
    let ro = open(&f, FsMode::Read).unwrap();
    assert_eq!(put(&ro, ""), Ok(()));
    assert_eq!(ro.write(&[]), Ok(()));
    assert_eq!(put(&ro, "x"), Err(ebadf_err()));
    assert_eq!(put(&Handle::stdin(), ""), Ok(()));
    assert_eq!(Handle::stdin().write(&[]), Ok(()));
}

/// After a failed write the error indicator is set, but `getLine` reports
/// only its own errors (LB-41): the next `getLine` reads its line, and
/// `read` returns its bytes. Natively the indicator is sticky: every later
/// `getLine` consumes its line and fails with `EBADF` (A722) until end of
/// file clears it.
#[test]
fn io_error_flag_after_a_failed_write() {
    let d = setup("err-flag");
    let f = p(&d, "ro.txt");
    fs::write(&f, "line1\nline2\nline3\n").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(get_line(&h).unwrap(), "line1\n");
    assert_eq!(put(&h, "x"), Err(ebadf_err()));
    assert_eq!(get_line(&h).unwrap(), "line2\n");
    assert_eq!(put(&h, "x"), Err(ebadf_err()));
    assert_eq!(read(&h, 3).unwrap(), b"lin");
    assert_eq!(get_line(&h).unwrap(), "e3\n");
    assert_eq!(read(&h, 3).unwrap(), b"");
    assert_eq!(get_line(&h).unwrap(), "");
}

/// After a short `read` at end of file the end-of-file indicator is sticky:
/// the next `getLine` (or small `read`) returns nothing once even though the
/// file has grown, then the new bytes; a `read` of a buffer or more reads
/// directly and sees them at once (A723).
#[test]
fn io_eof_flag_sticky() {
    let d = setup("eof-flag");
    let f = p(&d, "f.txt");
    let grow = |text: &str| {
        let a = open(&f, FsMode::Append).unwrap();
        put(&a, text).unwrap();
        a.flush().unwrap();
    };
    fs::write(&f, "abc").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(read(&h, 100).unwrap(), b"abc");
    grow("def\n");
    assert_eq!(get_line(&h).unwrap(), "");
    assert_eq!(get_line(&h).unwrap(), "def\n");
    fs::write(&f, "abc").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(read(&h, 100).unwrap(), b"abc");
    grow("def\n");
    assert_eq!(read(&h, 100).unwrap(), b"");
    assert_eq!(read(&h, 100).unwrap(), b"def\n");
    fs::write(&f, "abc").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(read(&h, 100).unwrap(), b"abc");
    grow("def\n");
    assert_eq!(read(&h, 1 << 20).unwrap(), b"def\n");
    fs::write(&f, "abc").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(get_line(&h).unwrap(), "abc");
    grow("def\n");
    assert_eq!(get_line(&h).unwrap(), "def\n");
}

/// A failing refill on a write-only handle writes its pending bytes first
/// (A724). LB-02: so does a read of a buffer or more, where glibc drops them;
/// on a `readWrite` handle the read then starts at the stream's position.
#[test]
fn io_read_after_write() {
    let d = setup("wo-read");
    let f = p(&d, "w.txt");
    let h = open(&f, FsMode::Write).unwrap();
    put(&h, "data").unwrap();
    assert_eq!(get_line(&h), Err(ebadf_err()));
    assert_eq!(fs::read(&f).unwrap(), b"data");
    put(&h, "more").unwrap();
    assert_eq!(read(&h, 5), Err(ebadf_err()));
    assert_eq!(fs::read(&f).unwrap(), b"datamore");
    put(&h, "kept").unwrap();
    assert_eq!(read(&h, 1 << 20), Err(ebadf_err()));
    assert_eq!(fs::read(&f).unwrap(), b"datamorekept");
    let g = p(&d, "rw.txt");
    fs::write(&g, "0123456789").unwrap();
    let h = open(&g, FsMode::ReadWrite).unwrap();
    put(&h, "AB").unwrap();
    assert_eq!(read(&h, 1 << 20).unwrap(), b"23456789");
    assert_eq!(fs::read(&g).unwrap(), b"AB23456789");
    // the judge's repro (judge-lean-bugs.md, item 2): read 5000 after putStr "abc"
    fs::write(&g, "0123456789").unwrap();
    let h = open(&g, FsMode::ReadWrite).unwrap();
    put(&h, "abc").unwrap();
    assert_eq!(read(&h, 5000).unwrap(), b"3456789");
    put(&h, "XYZ").unwrap();
    drop(h);
    assert_eq!(fs::read(&g).unwrap(), b"abc3456789XYZ");
}

/// When the pending bytes cannot be written out before a read, the read
/// fails with that write's error and reads nothing (LB-02). Below a buffer
/// this is native's own refill path (`/dev/full` opened `readWrite`:
/// `putStr "abc"`, `read 10` fails with ENOSPC; `read 10` again gives ten
/// zero bytes; `flush` succeeds; `putStr "def"`, `getLine` fails the same way;
/// `flush` succeeds). A direct read (4096, 5000) fails the same way, where
/// glibc drops the bytes and reads zeros. From leanrs's
/// `io_read_after_failed_write_out` (`local/follow-native`).
#[test]
fn io_read_after_failed_write_out() {
    if !std::path::Path::new("/dev/full").exists() {
        eprintln!("io_read_after_failed_write_out: no /dev/full, skipped");
        return;
    }
    let enospc = IoError::ResourceExhausted(None, 28, "no space left on device".into());
    for n in [10, 4096, 5000] {
        let rd = |h: &Handle| read(h, n).map(|v| (v.len(), v.iter().all(|&b| b == 0)));
        let h = open("/dev/full", FsMode::ReadWrite).unwrap();
        put(&h, "abc").unwrap();
        assert_eq!(rd(&h), Err(enospc.clone()), "read {n}");
        assert_eq!(rd(&h), Ok((n, true)), "read {n} again");
        h.flush().unwrap();
        put(&h, "def").unwrap();
        assert_eq!(get_line(&h), Err(enospc.clone()), "getLine after {n}");
        h.flush().unwrap();
    }
}

/// The same on standard output redirected to `/dev/full` (native: `putStr
/// "abc"`, `read 10` fails with ENOSPC; `putStr "abc"`, `read 5000` fails with
/// EBADF, glibc having dropped the bytes and read the write-only descriptor 1;
/// `read 5000` again fails with EBADF; `flush` succeeds). Here the first
/// `read 5000` fails with ENOSPC, the write-out's error (LB-02). From leanrs's
/// `io_stdout_read_after_failed_write_out`.
#[test]
fn io_stdout_read_after_failed_write_out() {
    let test = "io_stdout_read_after_failed_write_out";
    if child_case().as_deref() == Some(test) {
        // descriptor 1 becomes /dev/full here, after the harness's own output
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        rustix::stdio::dup2_stdout(&full).unwrap();
        let h = Handle::stdout();
        let len = |v: Vec<u8>| v.len().to_string();
        let lines = [
            r("putStr", put(&h, "abc"), unit),
            r("read 10", read(&h, 10), len),
            r("putStr 2", put(&h, "abc"), unit),
            r("read 5000", read(&h, 5000), len),
            r("read 5000 again", read(&h, 5000), len),
            r("flush", h.flush(), unit),
        ];
        let mut text = String::from("\n");
        for l in &lines {
            text.push_str(&format!("RESULT: {l}\n"));
        }
        let _ = std::io::stderr().write_all(text.as_bytes());
        exit::force_exit(0);
    }
    if !std::path::Path::new("/dev/full").exists() {
        eprintln!("{test}: no /dev/full, skipped");
        return;
    }
    let out = child(test, test, |_| {});
    let lines: Vec<String> = String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("RESULT: ").map(str::to_owned))
        .collect();
    assert_eq!(
        lines,
        vec![
            "putStr: ok ()",
            r#"read 10: err ResourceExhausted(None, 28, "no space left on device")"#,
            "putStr 2: ok ()",
            r#"read 5000: err ResourceExhausted(None, 28, "no space left on device")"#,
            r#"read 5000 again: err InvalidArgument(None, 9, "bad file descriptor")"#,
            "flush: ok ()",
        ],
        "{out:?}"
    );
}

/// On a FIFO opened `readWrite`, a direct read (a buffer or more) after a
/// write after a read: the seek back over the bytes read ahead fails
/// (`ESPIPE`), which is no failed write-out, so the pending bytes are dropped
/// and the read goes on, as glibc's direct read does (leanrs review F1;
/// native, leanrs's probe: `abc\n` then 8192 `z` in the FIFO, `getLine` gives
/// `abc\n`, `putStr "ghi\n"`, `read 4096` gives 4096 `z`, `read 4` gives
/// `zzzz`, the flush succeeds). A writer thread feeds the FIFO (it may hold
/// only one page), and `getLine` waits until a whole buffer is in it, as in
/// the native run. Under a deadline. From leanrs's
/// `io_fifo_direct_read_after_write_after_read`.
#[test]
fn io_fifo_direct_read_after_write_after_read() {
    let d = setup("fifo-direct-read");
    let fifo = p(&d, "fifo");
    if !Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|st| st.success())
    {
        eprintln!("io_fifo_direct_read_after_write_after_read: no mkfifo, skipped");
        return;
    }
    // the modelled `errno` after the direct read (native: EBADF, from the
    // failed `putStr`; the dropped `lseek` is never made natively, review
    // RIO1-17, the reviewer's FifoErrno), which native's `getLine` on the
    // read-only handle with its error indicator set reported; since LB-41
    // that `getLine` reads its line, and the model is read directly
    let xf = p(&d, "x.txt");
    fs::write(&xf, "hi\n").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let x = open(&xf, FsMode::Read).unwrap();
        assert!(put(&x, "x").is_err());
        let h = open(&fifo, FsMode::ReadWrite).unwrap();
        let watch = fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&fifo)
            .unwrap();
        let w = fifo.clone();
        std::thread::spawn(move || {
            let mut f = fs::OpenOptions::new().write(true).open(&w).unwrap();
            let _ = f.write_all(format!("abc\n{}", "z".repeat(8192)).as_bytes());
        });
        while rustix::io::ioctl_fionread(&watch).unwrap_or(0) < 4096 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let zs = |v: Vec<u8>| format!("{} all z {}", v.len(), v.iter().all(|&b| b == b'z'));
        let lines = vec![
            r("line", get_line(&h), |t| q(&t)),
            r("putStr 2", put(&h, "ghi\n"), unit),
            r("read 4096", read(&h, 4096), zs),
            format!("errno: {}", modelled_errno()),
            r("x getLine", get_line(&x), |t| q(&t)),
            r("read 4", read(&h, 4), zs),
            r("flush 2", h.flush(), unit),
        ];
        let _ = tx.send(lines);
    });
    let lines = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the scenario did not finish within 30 s");
    assert_eq!(
        lines,
        vec![
            r#"line: ok "abc\n""#,
            "putStr 2: ok ()",
            "read 4096: ok 4096 all z true",
            r#"errno: InvalidArgument(None, 9, "bad file descriptor")"#,
            r#"x getLine: ok "hi\n""#,
            "read 4: ok 4 all z true",
            "flush 2: ok ()",
        ]
    );
}

/// A write after a read moves the descriptor back over the unread bytes when
/// the bytes are written out (glibc's `new_do_write`).
#[test]
fn io_write_after_read_seeks_at_flush() {
    let d = setup("write-after-read");
    let f = p(&d, "rw.txt");
    fs::write(&f, "line1\nline2\n").unwrap();
    let h = open(&f, FsMode::ReadWrite).unwrap();
    assert_eq!(get_line(&h).unwrap(), "line1\n");
    put(&h, "X").unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"line1\nline2\n");
    h.flush().unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"line1\nXine2\n");
}

/// On a descriptor that cannot seek (a FIFO opened `readWrite`), a write after
/// a read fails at its flush with `ESPIPE` and its bytes are never written,
/// every later flush failing the same way, and a `getLine` then returns `""`
/// with no error, as compiled Lean does (leanrs's `fifo3`). In a child under a
/// deadline.
#[test]
fn io_fifo_write_after_read() {
    let test = "io_fifo_write_after_read";
    if let Some(fifo) = child_case().filter(|c| c.starts_with('/')) {
        let h = open(&fifo, FsMode::ReadWrite).unwrap();
        put(&h, "abc\ndef\n").unwrap();
        h.flush().unwrap();
        let mut lines = vec![r("line", get_line(&h), |t| q(&t))];
        lines.push(r("putStr", put(&h, "ghi\n"), unit));
        lines.push(r("flush", h.flush(), unit));
        lines.push(r("flush again", h.flush(), unit));
        lines.push(r("line2", get_line(&h), |t| q(&t)));
        lines.push(r("line3", get_line(&h), |t| q(&t)));
        report(&lines);
    }
    let d = setup("fifo-write-after-read");
    let fifo = p(&d, "fifo");
    if !Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|st| st.success())
    {
        eprintln!("{test}: no mkfifo, skipped");
        return;
    }
    let mut c = Command::new(std::env::current_exe().expect("test binary path"));
    c.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_VAR, &fifo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proc = c.spawn().expect("child test process");
    // read the child's output while it runs: a pipe may hold only one page
    let drain = |r: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut v);
            }
            v
        })
    };
    let out_t = drain(
        proc.stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn std::io::Read + Send>),
    );
    let err_t = drain(
        proc.stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn std::io::Read + Send>),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = proc.try_wait().expect("child status") {
            break st;
        }
        if Instant::now() > deadline {
            let _ = proc.kill();
            panic!("the child did not finish within 10 s: a getLine waits on the FIFO");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = Output {
        status,
        stdout: out_t.join().unwrap(),
        stderr: err_t.join().unwrap(),
    };
    assert_eq!(
        results(&out),
        vec![
            r#"line: ok "abc\n""#,
            "putStr: ok ()",
            r#"flush: err UnsupportedOperation(29, "invalid seek")"#,
            r#"flush again: err UnsupportedOperation(29, "invalid seek")"#,
            r#"line2: ok """#,
            r#"line3: ok """#,
        ]
    );
}

/// LB-09 (followed as native): a second `rewind` lands inside glibc's buffer,
/// so it serves the buffered bytes even though another writer has rewritten
/// the file; the first `rewind` (unknown offset) reads the file again.
#[test]
fn io_second_rewind_serves_buffer() {
    let d = setup("rewind-again");
    let f = p(&d, "s.txt");
    fs::write(&f, "old1\nold2\n").unwrap();
    let h = open(&f, FsMode::Read).unwrap();
    assert_eq!(get_line(&h).unwrap(), "old1\n");
    fs::write(&f, "mid1\nmid2\n").unwrap();
    h.rewind().unwrap();
    assert_eq!(get_line(&h).unwrap(), "mid1\n");
    fs::write(&f, "new1\nnew2\n").unwrap();
    h.rewind().unwrap();
    assert_eq!(get_line(&h).unwrap(), "mid1\n");
}

/// `truncate` on an `append` handle with bytes not yet written counts from
/// the end of the file (A725).
#[test]
fn io_truncate_append() {
    let d = setup("trunc-append");
    let f = p(&d, "a.txt");
    fs::write(&f, "0123456789").unwrap();
    let h = open(&f, FsMode::Append).unwrap();
    put(&h, "abc").unwrap();
    h.truncate().unwrap();
    h.flush().unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"0123456789\0\0\0abc");
    let g = p(&d, "a2.txt");
    let h = open(&g, FsMode::Append).unwrap();
    h.truncate().unwrap();
    put(&h, "zz").unwrap();
    h.flush().unwrap();
    h.truncate().unwrap();
    put(&h, "q").unwrap();
    h.truncate().unwrap();
    h.flush().unwrap();
    assert_eq!(fs::read(&g).unwrap(), b"zz\0q");
}

/// `exit` flushes the open files newest first, as glibc's `_IO_flush_all`
/// (A726; Lean leaves `AAA`).
#[test]
fn io_exit_flush_newest_first() {
    let test = "io_exit_flush_newest_first";
    if let Some(dir) = child_case().filter(|c| c.starts_with('/')) {
        let f = p(&dir, "o.txt");
        let h1 = open(&f, FsMode::Write).unwrap();
        put(&h1, "AAA").unwrap();
        let h2 = open(&f, FsMode::ReadWrite).unwrap();
        put(&h2, "BB").unwrap();
        let h3 = open(&f, FsMode::ReadWrite).unwrap();
        put(&h3, "C").unwrap();
        let _keep = (h1, h2, h3);
        exit::exit(0);
    }
    let d = setup("exit-order");
    let out = child(test, &d, |_| {});
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(fs::read(p(&d, "o.txt")).unwrap(), b"AAA");
}

/// Standard input is read through its own buffer: `flush` gives its
/// read-ahead back and moves descriptor 0 back, so the next `getLine` reads
/// the file again (`fflush(stdin)`; leanrs's `stdinflush`: `a\n`, then
/// `YY\n`, `Z\n`).
#[test]
fn io_stdin_flush() {
    let test = "io_stdin_flush";
    if let Some(dir) = child_case().filter(|c| c.starts_with('/')) {
        let h = Handle::stdin();
        let mut lines = vec![get_line(&h).unwrap()];
        fs::write(p(&dir, "in.txt"), "X\nYY\nZ\n").unwrap();
        h.flush().unwrap();
        lines.push(get_line(&h).unwrap());
        lines.push(get_line(&h).unwrap());
        report(&lines.iter().map(|l| format!("{l:?}")).collect::<Vec<_>>());
    }
    let d = setup("stdin-flush");
    fs::write(p(&d, "in.txt"), "a\nb\nc\n").unwrap();
    let input = fs::File::open(p(&d, "in.txt")).unwrap();
    let out = child(test, &d, |c| {
        c.stdin(input);
    });
    assert_eq!(results(&out), vec!["\"a\\n\"", "\"YY\\n\"", "\"Z\\n\""]);
}

/// The read-ahead of standard input is given back at exit (`_IO_unbuffer_all`
/// syncs it), so the next reader of a seekable stdin starts where the program
/// stopped (lean2rr's `RtStdioStdinAhead`).
#[test]
fn io_stdin_left_for_next_process() {
    let test = "io_stdin_left_for_next_process";
    if child_case().as_deref() == Some("ahead") {
        let h = Handle::stdin();
        let l = get_line(&h).unwrap();
        report(&[q(&l)]);
    }
    let d = setup("stdin-ahead");
    fs::write(p(&d, "in.txt"), "first\nsecond\nthird\n").unwrap();
    let input = fs::File::open(p(&d, "in.txt")).unwrap();
    let mut keep = input.try_clone().unwrap();
    let out = child(test, "ahead", |c| {
        c.stdin(input);
    });
    assert_eq!(results(&out), vec![r#""first\n""#]);
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut keep, &mut rest).unwrap();
    assert_eq!(rest, "second\nthird\n");
}

/// Standard output on a terminal is flushed before a refill only when the
/// stream read is a terminal too (glibc's `_IO_new_file_underflow`): with
/// standard input a pipe, `Name: ` stays buffered across `getLine` and the
/// terminal shows `E` first (compiled Lean: `EName: got "xy\n"`). Under
/// `script`, which gives the child a terminal; skipped without `script`.
#[test]
fn io_tty_flush_rule() {
    let test = "io_tty_flush_rule";
    if child_case().as_deref() == Some(test) {
        let (out, err, inp) = (Handle::stdout(), Handle::stderr(), Handle::stdin());
        put(&out, "Name: ").unwrap();
        let line = get_line(&inp).unwrap();
        put(&err, "E").unwrap();
        put(&out, &format!("got {line:?}\n")).unwrap();
        exit::exit(0);
    }
    let exe = std::env::current_exe().expect("test binary path");
    let cmd = format!(
        "printf 'xy\\n' | env {CHILD_VAR}={test} '{}' {test} --exact --nocapture --test-threads=1",
        exe.display()
    );
    let Ok(out) = Command::new("script")
        .args(["-q", "-e", "-c", &cmd, "/dev/null"])
        .stdin(Stdio::null())
        .output()
    else {
        eprintln!("{test}: no `script`, skipped");
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("EName: got \"xy\\n\""),
        "terminal transcript: {text:?}"
    );
}
