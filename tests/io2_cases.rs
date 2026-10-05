//! The io-2 program cases (`tests/cases/{process,temp,uvsys,streams}`, and
//! `io/temp_file_error`), and those of `tests/cases/{debug,clock,panics}`,
//! on the crate: each case has a twin here, a Rust function making the same
//! calls through `lean_runtime` as the case's Lean program makes through
//! Lean's runtime, with what a translator's glue adds (`IO.println` is one
//! `putStr` of the line and `\n` on the current standard output;
//! `Handle.readToEnd` reads 1024 bytes at a time and checks UTF-8;
//! `IO.Process.run` is its Lean definition over `output`; an uncaught error
//! is `show_error` of `IO.Error.toString`, then exit status 1).
//!
//! The test runs `scripts/cases.py check` (the checker translators use) on
//! hard links of this binary named after each case, so a twin keeps
//! `argv[0]`, `IO.appPath` and the environment the checker gives it; every
//! twin's stdout, stderr and exit code must equal the case's expected
//! outcome: native Lean 4.34.0's, or the correct one where native is wrong
//! (LB-03, LB-14, LB-15, LB-16, LB-17 in `docs/lean-bugs.md`; native's is
//! then in the case's `native` field), or the documented alternative where
//! LB-17's fix costs a descriptor (LIO2-05, `pipe_null_two_free`).
//!
//! With the feature `proc-title`, the crate's own ELF constructor
//! (`io::argv_title`) hands it the arguments, so `setProcessTitle` writes
//! them as natively (`title_cmdline`, `title_in_initializer`,
//! `process_title`): no glue takes part, and this binary links the
//! constructor as any other binary does. Without the feature,
//! `setProcessTitle` fails with `ENOBUFS`, so the twins of the cases that
//! set a title ([`NEED_PROC_TITLE`]) are not checked; `title_via_loader` is
//! checked in both builds, since its accepted alternative (LQ1-01) is that
//! `ENOBUFS`.
//!
//! In a threads build (feature `threads`) every twin runs inside a task,
//! on a worker thread, which `main` waits for (`tests/in_task/mod.rs`):
//! the same expected outcomes, checked on a thread that is not `main`'s.
//!
//! The binary runs without libtest (`harness = false`).

use std::cell::RefCell;
use std::rc::Rc;

use lean_runtime::io::process::{self, Child, SpawnArgs, Stdio, StdioConfig};
use lean_runtime::io::streams::{self, StdStream};
use lean_runtime::io::{
    debug, env as lenv, exit, fs as lfs, temp, uvsys, FsMode, Handle, IoError, StoppingSink,
};
use lean_runtime::semantics::array;
use lean_runtime::semantics::panic::InternalPanic;

#[cfg(feature = "threads")]
mod in_task;

// ---- the glue a translator adds ----

type R<T> = Result<T, IoError>;

/// `IO.FS.Stream`, as the twins need it: a handle's (`ofHandle`) or a
/// buffer's (`ofBuffer`).
#[derive(Clone)]
enum Stream {
    Handle(Handle),
    Buffer(Rc<RefCell<Vec<u8>>>),
}

impl Stream {
    fn put_str(&self, s: &[u8]) -> R<()> {
        match self {
            Stream::Handle(h) => h.put_str(s),
            Stream::Buffer(b) => {
                b.borrow_mut().extend_from_slice(s);
                Ok(())
            }
        }
    }

    fn flush(&self) -> R<()> {
        match self {
            Stream::Handle(h) => h.flush(),
            Stream::Buffer(_) => Ok(()),
        }
    }
}

fn stdout() -> Stream {
    streams::current(StdStream::Stdout, || Stream::Handle(Handle::stdout()))
}

fn stderr() -> Stream {
    streams::current(StdStream::Stderr, || Stream::Handle(Handle::stderr()))
}

fn set_stdout(s: Stream) -> Stream {
    streams::set_stdout(s, || Stream::Handle(Handle::stdout()))
}

fn set_stderr(s: Stream) -> Stream {
    let put = s.clone();
    streams::set_stderr(
        s,
        Rc::new(move |l: &[u8]| {
            let _ = put.put_str(l);
        }),
        || Stream::Handle(Handle::stderr()),
    )
}

fn print(s: &str) -> R<()> {
    stdout().put_str(s.as_bytes())
}

fn println(s: &str) -> R<()> {
    print(&format!("{s}\n"))
}

fn eprintln(s: &str) -> R<()> {
    stderr().put_str(format!("{s}\n").as_bytes())
}

/// Lean's `String.quote` (`repr` of a string).
fn quote(s: &str) -> String {
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

fn down(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Lean's `IO.Error.toString` (`Init/System/IOError.lean`).
fn to_string(e: &IoError) -> String {
    use IoError as E;
    let fopen = |gist: &str, f: &str, c: &u32, d: Option<&String>| match d {
        Some(d) => format!("{} (error code: {c}, {})\n  file: {f}", down(gist), down(d)),
        None => format!("{} (error code: {c})\n  file: {f}", down(gist)),
    };
    let other = |gist: &str, c: &u32, d: Option<&String>| match d {
        Some(d) => format!("{} (error code: {c}, {})", down(gist), down(d)),
        None => format!("{} (error code: {c})", down(gist)),
    };
    match e {
        E::UnexpectedEof => "end of file".into(),
        E::InappropriateType(Some(f), c, d) => fopen("inappropriate type", f, c, Some(d)),
        E::InappropriateType(None, c, d) => other("inappropriate type", c, Some(d)),
        E::Interrupted(f, c, d) => fopen("interrupted system call", f, c, Some(d)),
        E::InvalidArgument(Some(f), c, d) => fopen("invalid argument", f, c, Some(d)),
        E::InvalidArgument(None, c, d) => other("invalid argument", c, Some(d)),
        E::NoFileOrDirectory(f, c, _) => fopen("no such file or directory", f, c, None),
        E::NoSuchThing(Some(f), c, d) => fopen("no such thing", f, c, Some(d)),
        E::NoSuchThing(None, c, d) => other("no such thing", c, Some(d)),
        E::PermissionDenied(Some(f), c, d) => fopen(d, f, c, None),
        E::PermissionDenied(None, c, d) => other(d, c, None),
        E::ResourceExhausted(Some(f), c, d) => fopen("resource exhausted", f, c, Some(d)),
        E::ResourceExhausted(None, c, d) => other("resource exhausted", c, Some(d)),
        E::AlreadyExists(None, c, d) => other("already exists", c, Some(d)),
        E::AlreadyExists(Some(f), c, d) => fopen("already exists", f, c, Some(d)),
        E::OtherError(c, d) => other(d, c, None),
        E::ResourceBusy(c, d) => other("resource busy", c, Some(d)),
        E::ResourceVanished(c, d) => other("resource vanished", c, Some(d)),
        E::HardwareFault(c, _) => other("hardware fault", c, None),
        E::IllegalOperation(c, d) => other("illegal operation", c, Some(d)),
        E::ProtocolError(c, d) => other("protocol error", c, Some(d)),
        E::TimeExpired(c, d) => other("time expired", c, Some(d)),
        E::UnsatisfiedConstraints(c, _) => other("directory not empty", c, None),
        E::UnsupportedOperation(c, d) => other("unsupported operation", c, Some(d)),
        E::UserError(m) => m.clone(),
    }
}

/// The end of a native program: `lean_finalize_task_manager` (the io
/// layer's dedicated tasks, `exit::after_main`), `main`'s result, then C's
/// `exit`.
fn finish(r: R<()>) -> ! {
    exit::after_main();
    match r {
        Ok(()) => exit::exit(0),
        // `lean_io_result_show_error`, then status 1 (`io::panic::uncaught`)
        Err(e) => lean_runtime::io::panic::uncaught(
            to_string(&e).as_bytes(),
            &mut lean_runtime::io::panic::Native,
        ),
    }
}

/// `try ... catch e => f(e)`.
fn catch<T>(r: R<T>, ok: impl FnOnce(T) -> String, err: impl FnOnce(String) -> String) -> String {
    match r {
        Ok(v) => ok(v),
        Err(e) => err(to_string(&e)),
    }
}

/// `toString` of an `Option` (`(some x)`, `none`).
fn opt<T: std::fmt::Display>(o: Option<T>) -> String {
    match o {
        Some(x) => format!("(some {x})"),
        None => "none".to_owned(),
    }
}

/// `Handle.readToEnd`: `read 1024` until empty, then UTF-8.
fn read_to_end(h: &Option<Handle>) -> R<String> {
    let h = h.as_ref().expect("a piped stream");
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = h.read(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    String::from_utf8(data)
        .map_err(|_| IoError::user_error("Tried to read from handle containing non UTF-8 data."))
}

fn get_line(h: &Option<Handle>) -> R<String> {
    let mut v = Vec::new();
    h.as_ref().expect("a piped stream").get_line(&mut v)?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// `IO.FS.writeFile`.
fn write_file(path: &str, content: &str) -> R<()> {
    Handle::open(path.as_bytes(), FsMode::Write)?.put_str(content.as_bytes())
}

/// `IO.FS.readFile` (`readBinFile`, then UTF-8).
fn read_file(path: &str) -> R<String> {
    let h = Handle::open(path.as_bytes(), FsMode::Read)?;
    read_to_end(&Some(h))
}

/// `IO.setAccessRights` with the user's bits only.
fn set_rights(path: &str, r: bool, w: bool, x: bool) -> R<()> {
    lfs::set_access_rights(
        path.as_bytes(),
        (u32::from(r) << 8) | (u32::from(w) << 7) | (u32::from(x) << 6),
    )
}

fn cwd() -> String {
    let mut v = Vec::new();
    lfs::current_dir(&mut v).unwrap();
    String::from_utf8(v).unwrap()
}

/// A `SpawnArgs` with Lean's defaults.
struct Spawn {
    cmd: Vec<u8>,
    args: Vec<Vec<u8>>,
    cwd: Option<Vec<u8>>,
    env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    inherit_env: bool,
    setsid: bool,
    cfg: StdioConfig,
}

fn cmd(c: &str) -> Spawn {
    Spawn {
        cmd: c.as_bytes().to_vec(),
        args: Vec::new(),
        cwd: None,
        env: Vec::new(),
        inherit_env: true,
        setsid: false,
        cfg: StdioConfig {
            stdin: Stdio::Inherit,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    }
}

impl Spawn {
    fn args(mut self, a: &[&str]) -> Spawn {
        self.args = a.iter().map(|s| s.as_bytes().to_vec()).collect();
        self
    }
    fn cwd(mut self, d: &str) -> Spawn {
        self.cwd = Some(d.as_bytes().to_vec());
        self
    }
    fn env(mut self, e: &[(&str, Option<&str>)]) -> Spawn {
        self.env = e
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.map(|v| v.as_bytes().to_vec())))
            .collect();
        self
    }
    fn no_inherit(mut self) -> Spawn {
        self.inherit_env = false;
        self
    }
    fn setsid(mut self, s: bool) -> Spawn {
        self.setsid = s;
        self
    }
    fn stdio(mut self, i: Stdio, o: Stdio, e: Stdio) -> Spawn {
        self.cfg = StdioConfig {
            stdin: i,
            stdout: o,
            stderr: e,
        };
        self
    }
    fn with<T>(&self, f: impl FnOnce(&SpawnArgs) -> T) -> T {
        let args: Vec<&[u8]> = self.args.iter().map(Vec::as_slice).collect();
        let env: Vec<(&[u8], Option<&[u8]>)> = self
            .env
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_deref()))
            .collect();
        f(&SpawnArgs {
            cmd: &self.cmd,
            args: &args,
            cwd: self.cwd.as_deref(),
            env: &env,
            inherit_env: self.inherit_env,
            setsid: self.setsid,
        })
    }
    fn spawn(&self) -> R<Child> {
        self.with(|a| process::spawn(self.cfg, a))
    }
    /// `IO.Process.output`.
    fn output(&self, input: Option<&str>) -> R<Output> {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = self.with(|a| process::output(a, input.map(str::as_bytes), &mut o, &mut e))?;
        Ok(Output {
            exit_code: code,
            stdout: String::from_utf8(o).unwrap(),
            stderr: String::from_utf8(e).unwrap(),
        })
    }
    /// `IO.Process.run`.
    fn run(&self) -> R<String> {
        let out = self.output(None)?;
        if out.exit_code != 0 {
            return Err(IoError::user_error(&format!(
                "process '{}' exited with code {}\nstderr:\n{}",
                String::from_utf8_lossy(&self.cmd),
                out.exit_code,
                out.stderr
            )));
        }
        Ok(out.stdout)
    }
}

struct Output {
    exit_code: u32,
    stdout: String,
    stderr: String,
}

use Stdio::{Inherit, Null, Piped};

// ---- process ----

fn proc_spawn(_: &[String]) -> R<()> {
    let child = cmd("cat").stdio(Piped, Piped, Null).spawn()?;
    let Child {
        stdin,
        stdout,
        process: p,
        ..
    } = child;
    let stdin = stdin.unwrap();
    stdin.put_str(b"through cat\nsecond line\n")?;
    stdin.flush()?;
    drop(stdin);
    let out = read_to_end(&stdout)?;
    let code = p.wait()?;
    println(&format!("cat: {code} {}", quote(&out)))?;
    let child = cmd("sh")
        .args(&["-c", "echo e1 >&2; echo o1; exit 7"])
        .stdio(Null, Piped, Piped)
        .spawn()?;
    let err = read_to_end(&child.stderr)?;
    let out = read_to_end(&child.stdout)?;
    let code = child.process.wait()?;
    println(&format!("sh: {code} {} {}", quote(&out), quote(&err)))?;
    println(&catch(
        child.process.wait(),
        |a| format!("second wait: {a}"),
        |e| format!("second wait error: {e}"),
    ))?;
    println(&catch(
        child.process.try_wait(),
        |t| format!("tryWait after wait: {}", opt(t)),
        |e| format!("tryWait after wait error: {e}"),
    ))?;
    let child = cmd("sleep").args(&["30"]).stdio(Null, Null, Null).spawn()?;
    println(&format!("pid positive: {}", child.process.pid() > 0))?;
    child.process.kill()?;
    let code = child.process.wait()?;
    println(&format!("killed: {code}"))?;
    println(&catch(
        child.process.kill(),
        |()| "kill after wait: ok".to_owned(),
        |e| format!("kill after wait error: {e}"),
    ))?;
    let child = cmd("true").stdio(Null, Null, Null).spawn()?;
    let mut status = None;
    while status.is_none() {
        status = child.process.try_wait()?;
        if status.is_none() {
            lenv::sleep(5);
        }
    }
    println(&format!("tryWait: {}", opt(status)))?;
    let child = cmd("leanrs-no-such-program-xyz")
        .stdio(Piped, Piped, Piped)
        .spawn()?;
    let err = read_to_end(&child.stderr)?;
    let out = read_to_end(&child.stdout)?;
    let code = child.process.wait()?;
    println(&format!(
        "missing piped: {code} {} {}",
        quote(&out),
        quote(&err)
    ))?;
    let child = cmd("leanrs-no-such-program-xyz")
        .stdio(Null, Inherit, Inherit)
        .spawn()?;
    let code = child.process.wait()?;
    println(&format!("missing inherited: {code}"))?;
    print("before child; ")?;
    let child = cmd("echo").args(&["child line"]).spawn()?;
    let code = child.process.wait()?;
    println(&format!("after child {code}"))?;
    print("unflushed; ")?;
    let child = cmd("echo")
        .args(&["second child"])
        .stdio(Piped, Inherit, Inherit)
        .spawn()?;
    let code = child.process.wait()?;
    println(&format!("after second child {code}"))?;
    for setsid in [false, true] {
        let child = cmd("sh")
            .args(&["-c", "set -- $(cat /proc/$$/stat); if [ \"$6\" = \"$$\" ]; then echo leader; else echo member; fi"])
            .setsid(setsid)
            .stdio(Null, Piped, Null)
            .spawn()?;
        let out = read_to_end(&child.stdout)?;
        let code = child.process.wait()?;
        println(&format!("setsid {setsid}: {code} {}", quote(&out)))?;
    }
    Ok(())
}

fn report(label: &str, o: R<Output>) -> R<()> {
    let o = o?;
    println(&format!(
        "{label}: exit {} out {} err {}",
        o.exit_code,
        quote(&o.stdout),
        quote(&o.stderr)
    ))
}

fn proc_output(_: &[String]) -> R<()> {
    report(
        "echo",
        cmd("echo").args(&["hello", "pipe world"]).output(None),
    )?;
    report("cat", cmd("cat").output(Some("line one\nline two\n")))?;
    report("cat-empty", cmd("cat").output(None))?;
    report(
        "sh",
        cmd("sh")
            .args(&["-c", "echo to-out; echo to-err >&2; exit 3"])
            .output(None),
    )?;
    report(
        "missing",
        cmd("leanrs-no-such-program-xyz").args(&["a"]).output(None),
    )?;
    report("slash-missing", cmd("./no/such/prog").output(None))?;
    report(
        "cwd",
        cmd("sh")
            .args(&["-c", "basename \"$PWD\""])
            .cwd("sub")
            .output(None),
    )?;
    report("bad-cwd", cmd("pwd").cwd("no-such-dir").output(None))?;
    report(
        "env-set",
        cmd("sh")
            .args(&["-c", "echo \"[$IOFIX_A][$IOFIX_B][${IOFIX_PARENT-unset}]\""])
            .env(&[
                ("IOFIX_A", Some("alpha")),
                ("IOFIX_B", Some("beta")),
                ("IOFIX_A", Some("again")),
                ("IOFIX_PARENT", None),
            ])
            .output(None),
    )?;
    report(
        "env-inherit",
        cmd("sh")
            .args(&["-c", "echo \"[${IOFIX_PARENT-unset}]\""])
            .output(None),
    )?;
    report(
        "env-clear",
        cmd("/usr/bin/env")
            .no_inherit()
            .env(&[("ONLY", Some("this one"))])
            .output(None),
    )?;
    report(
        "env-order",
        cmd("/usr/bin/env")
            .env(&[
                ("ZZ_NEW", Some("1")),
                ("HOME", Some("home2")),
                ("PATH", None),
                ("AA_NEW", Some("2")),
                ("ZZ_NEW", None),
                ("ZZ_NEW", Some("3")),
            ])
            .output(None),
    )?;
    report(
        "env-bad-names",
        cmd("sh")
            .args(&["-c", "echo \"[${IOFIX_OK-unset}]\""])
            .env(&[
                ("", Some("x")),
                ("A=B", Some("y")),
                ("IOFIX_OK", Some("ok")),
                ("=", None),
            ])
            .output(None),
    )?;
    report(
        "nul-args",
        cmd("echo\0ignored")
            .args(&["one\0two", "three"])
            .output(None),
    )?;
    report(
        "nul-env",
        cmd("sh")
            .args(&["-c", "echo \"[$IOFIX_NUL]\""])
            .env(&[("IOFIX_NUL", Some("before\0after"))])
            .output(None),
    )?;
    report(
        "env-nopath",
        cmd("env")
            .env(&[("PATH", None), ("HOME", None)])
            .output(None),
    )?;
    report(
        "big",
        cmd("sh")
            .args(&[
                "-c",
                "i=0; while [ $i -lt 3000 ]; do echo line-$i; echo err-$i >&2; i=$((i+1)); done",
            ])
            .output(None),
    )?;
    report("signal", cmd("sh").args(&["-c", "kill -9 $$"]).output(None))?;
    let s = cmd("printf").args(&["%s|%s", "x", "y z"]).run()?;
    println(&format!("run: {}", quote(&s)))?;
    println(&catch(
        cmd("sh")
            .args(&["-c", "echo partial; echo why >&2; exit 2"])
            .run(),
        |_| "run: no error".into(),
        |e| format!("run error: {e}"),
    ))?;
    println(&catch(
        cmd("leanrs-no-such-program-xyz").run(),
        |_| "run: no error".into(),
        |e| format!("run missing: {e}"),
    ))?;
    report(
        "script",
        cmd("sub/noshebang").args(&["x", "y"]).output(None),
    )?;
    report(
        "script-path",
        cmd("noshebang")
            .args(&["z"])
            .env(&[("PATH", Some("/nonexistent:sub"))])
            .output(None),
    )?;
    println(&catch(
        cmd("printf").args(&["\\377\\376"]).output(None),
        |o| format!("invalid utf8: {}", o.exit_code),
        |e| format!("invalid utf8 error: {e}"),
    ))
}

fn report_flush(label: &str, o: R<Output>) -> R<()> {
    report(label, o)?;
    stdout().flush()
}

/// `IO.FileRight` with the user's bits only, for the `mode` helpers.
fn proc_inherit(args: &[String]) -> R<()> {
    if args == ["envchild"] {
        let show = |label: &str, o: Output| println(&format!("{label}: {}", quote(&o.stdout)));
        let environ = || {
            let mut v = Vec::new();
            uvsys::os_environ(|k, val| {
                v.push(format!(
                    "({}, {})",
                    String::from_utf8_lossy(k),
                    String::from_utf8_lossy(val)
                ))
            });
            format!("[{}]", v.join(", "))
        };
        show(
            "noinherit",
            cmd("/usr/bin/env")
                .no_inherit()
                .env(&[("Z", Some("1"))])
                .output(None)?,
        )?;
        println(&format!("after noinherit: {}", environ()))?;
        show(
            "changed",
            cmd("/usr/bin/env")
                .env(&[("A", None), ("B", Some("9"))])
                .output(None)?,
        )?;
        println(&format!("after changed: {}", environ()))?;
        return show("plain", cmd("/usr/bin/env").output(None)?);
    }
    report_flush(
        "sigpipe",
        cmd("sh")
            .args(&[
                "-c",
                "(yes 2>/dev/null; echo \"yes exited $?\" >&2) | head -n 1",
            ])
            .output(None),
    )?;
    lfs::create_dir(b"locked")?;
    set_rights("locked", true, true, false)?;
    report_flush("cwd-no-search", cmd("pwd").cwd("locked").output(None))?;
    report_flush(
        "cwd-no-search-missing-prog",
        cmd("leanrs-no-such-program-xyz").cwd("locked").output(None),
    )?;
    lfs::create_dir(b"searchonly")?;
    set_rights("searchonly", false, false, true)?;
    report_flush(
        "cwd-search-only",
        cmd("sh")
            .args(&["-c", "basename \"$PWD\""])
            .cwd("searchonly")
            .output(None),
    )?;
    write_file("plain", "x")?;
    report_flush("cwd-file", cmd("pwd").cwd("plain").output(None))?;
    report_flush("cwd-through-file", cmd("pwd").cwd("plain/sub").output(None))?;
    report_flush("cwd-empty", cmd("pwd").cwd("").output(None))?;
    set_rights("locked", true, true, true)?;
    set_rights("searchonly", true, true, true)?;
    lfs::remove_dir(b"locked")?;
    lfs::remove_dir(b"searchonly")?;
    lfs::remove_file(b"plain")?;
    let mut me = Vec::new();
    lenv::app_path(&mut me)?;
    let reexec = "import ctypes, sys\nlibc = ctypes.CDLL(None)\nexe = sys.argv[1].encode()\nenv = [b'=x', b'A=1', b'=a=b', b'B=2', b'=', b'D=4', b'B=dup', b'noequals', b'PATH=/usr/bin:/bin']\nargv = (ctypes.c_char_p * 3)(exe, b'envchild', None)\nenvp = (ctypes.c_char_p * (len(env) + 1))(*env, None)\nlibc.execve(exe, argv, envp)\nsys.exit(127)\n";
    report_flush(
        "malformed environ",
        cmd("python3")
            .args(&["-c", reexec, &String::from_utf8(me).unwrap()])
            .output(None),
    )?;
    println("end")
}

fn output_case(label: &str, a: Spawn, input: &str) -> R<()> {
    let line = catch(
        a.output(Some(input)),
        |o| {
            format!(
                "{label}: exit {} out {} err {}",
                o.exit_code,
                quote(&o.stdout),
                quote(&o.stderr)
            )
        },
        |e| format!("{label}: error {e}"),
    );
    println(&line)?;
    stdout().flush()
}

fn failed_child_stdin(args: &[String]) -> R<()> {
    let input = args.first().map_or("data", String::as_str);
    output_case("output", cmd("no-such-program-xyz"), input)?;
    output_case("output-empty", cmd("no-such-program-xyz"), "")?;
    output_case("output-bad-cwd", cmd("cat").cwd("no-such-dir"), input)?;
    output_case("output-big", cmd("no-such-program-xyz"), &"x".repeat(70000))?;
    let c = cmd("no-such-program-xyz")
        .stdio(Piped, Null, Null)
        .spawn()?;
    let p = c.process.take_stdin();
    let stdin = c.stdin.unwrap();
    let r = stdin.put_str(input.as_bytes()).and_then(|()| stdin.flush());
    println(&catch(
        r,
        |()| "spawn stdin write: ok".into(),
        |e| format!("spawn stdin write: error {e}"),
    ))?;
    println(&format!("code {}", p.wait()?))?;
    let c = cmd("no-such-program-xyz")
        .stdio(Piped, Null, Null)
        .spawn()?;
    let stdin = c.stdin.as_ref().unwrap();
    let r = stdin
        .put_str("y".repeat(70000).as_bytes())
        .and_then(|()| stdin.flush());
    println(&catch(
        r,
        |()| "spawn big write: ok".into(),
        |e| format!("spawn big write: error {e}"),
    ))?;
    println(&format!("code {}", c.process.wait()?))
}

fn output_fields(_: &[String]) -> R<()> {
    let o = cmd("echo").args(&["hi"]).output(None)?;
    let d = cmd("echo").args(&["bind"]).output(None)?;
    println(&format!(
        "{} {} {} {} {} {} {} {}",
        quote("def"),
        9,
        "some \"closure\"",
        quote(&d.stdout.chars().count().to_string()),
        o.exit_code + 1,
        quote(&o.stdout),
        quote("two"),
        o.exit_code
    ))?;
    write_file("f.txt", "abc")?;
    let m = lfs::metadata(b"f.txt")?;
    println(&format!("{} {}", m.byte_size + 1, m.byte_size))?;
    lfs::remove_file(b"f.txt")
}

fn rt_process(_: &[String]) -> R<()> {
    let o = cmd("sh")
        .args(&["-c", "echo out; echo err >&2; exit 3"])
        .output(None)?;
    println(&format!(
        "output: code {} stdout {} stderr {}",
        o.exit_code,
        quote(&o.stdout),
        quote(&o.stderr)
    ))?;
    let r = cmd("echo").args(&["run", "me"]).run()?;
    println(&format!("run: {}", quote(&r)))?;
    let e = cmd("sh")
        .args(&["-c", "echo \"[$RT_A][$RT_B]\""])
        .env(&[("RT_A", Some("set")), ("RT_B", None)])
        .output(None)?;
    println(&format!("env: {}", quote(&e.stdout)))?;
    let c = cmd("pwd").cwd("/").output(None)?;
    println(&format!("cwd: {}", quote(&c.stdout)))?;
    let bad = cmd("rt-process-no-such-command").output(None)?;
    println(&format!(
        "missing: code {} stderr {}",
        bad.exit_code,
        quote(&bad.stderr)
    ))?;
    let child = cmd("tr")
        .args(&["a-z", "A-Z"])
        .stdio(Piped, Piped, Inherit)
        .spawn()?;
    let stdin = child.stdin.unwrap();
    stdin.put_str(b"piped through tr\n")?;
    stdin.flush()?;
    drop(stdin);
    let out = read_to_end(&child.stdout)?;
    println(&format!(
        "tr: {} code {}",
        quote(&out),
        child.process.wait()?
    ))?;
    let sleeper = cmd("sleep").args(&["30"]).spawn()?;
    println(&format!(
        "tryWait while running: {}",
        sleeper.process.try_wait()?.is_some()
    ))?;
    sleeper.process.kill()?;
    println(&format!("killed: {}", sleeper.process.wait()?))?;
    let quick = cmd("true").spawn()?;
    quick.process.wait()?;
    println(&catch(
        quick.process.wait(),
        |_| "second wait ok".into(),
        |e| format!("second wait: {e}"),
    ))
}

fn show_out(tag: &str, o: Output) -> R<()> {
    println(&format!(
        "{tag}: code {} out {} err {}",
        o.exit_code,
        quote(&o.stdout),
        quote(&o.stderr)
    ))
}

/// The cases' check that a killed process is gone: it polls for up to 3 s, since a killed
/// process may take a moment to exit (and to be reaped) on a loaded host.
fn gone_probe(pid: &str) -> String {
    format!(
        "i=0; while kill -0 {pid} 2>/dev/null && [ $i -lt 60 ]; do sleep 0.05; i=$((i+1)); done; \
         kill -0 {pid} 2>/dev/null && echo alive || echo gone"
    )
}

fn rt_process_spawn(argv: &[String]) -> R<()> {
    println("pending")?;
    show_out(
        "missing",
        cmd("rt-process-spawn-no-such-command").output(None)?,
    )?;
    show_out(
        "badcwd",
        cmd("pwd")
            .cwd("/rt-process-spawn-no-such-dir")
            .output(None)?,
    )?;
    show_out(
        "env",
        cmd("sh")
            .args(&["-c", "echo \"[${A-unset}][${B-unset}][${C-unset}]\""])
            .env(&[
                ("A", Some("1")),
                ("A", Some("2")),
                ("B", Some("")),
                ("C", Some("x")),
                ("C", None),
            ])
            .output(None)?,
    )?;
    show_out(
        "noinherit",
        cmd("/usr/bin/env")
            .env(&[("ONLY", Some("me"))])
            .no_inherit()
            .output(None)?,
    )?;
    show_out(
        "signal",
        cmd("sh").args(&["-c", "kill -TERM $$"]).output(None)?,
    )?;
    show_out(
        "input",
        cmd("sh")
            .args(&["-c", "cat; echo done >&2"])
            .output(Some("fed\n"))?,
    )?;
    let c = cmd("sh")
        .args(&["-c", "sleep 0.2; echo late || echo write-failed >&2"])
        .stdio(Inherit, Piped, Inherit)
        .spawn()?;
    println(&format!("late writer: {}", c.process.wait()?))?;
    let s = cmd("sleep").args(&["30"]).spawn()?;
    println(&format!("tryWait running: {}", opt(s.process.try_wait()?)))?;
    s.process.kill()?;
    println(&format!("killed: {}", s.process.wait()?))?;
    println(&catch(
        s.process.try_wait(),
        |t| format!("tryWait after reap: {}", opt(t)),
        |e| format!("tryWait after reap: {e}"),
    ))?;
    println(&catch(
        s.process.kill(),
        |()| "kill after reap ok".into(),
        |e| format!("kill after reap: {e}"),
    ))?;
    let r = cmd("sh").args(&["-c", "sleep 0.1; exit 7"]).spawn()?;
    let mut res = None;
    while res.is_none() {
        res = r.process.try_wait()?;
        lenv::sleep(20);
    }
    println(&format!("polled: {}", opt(res)))?;
    let g = cmd("sh")
        .args(&["-c", "sleep 30 & echo $!; wait"])
        .stdio(Inherit, Piped, Inherit)
        .setsid(true)
        .spawn()?;
    let line = get_line(&g.stdout)?;
    g.process.kill()?;
    println(&format!("group killed: {}", g.process.wait()?))?;
    let chk = cmd("sh")
        .args(&["-c", &gone_probe(line.trim())])
        .output(None)?;
    println(&format!("grandchild: {}", chk.stdout.trim()))?;
    if let Err(e) = cmd("sh").args(&["-c", "echo oops >&2; exit 3"]).run() {
        println(&format!("run failed: {}", to_string(&e)))?;
    }
    let out = if argv.is_empty() { Piped } else { Inherit };
    let mut codes = Vec::new();
    let mut cs = Vec::new();
    for i in 1..=3 {
        cs.push(
            cmd("sh")
                .args(&["-c", &format!("exit {i}")])
                .stdio(Inherit, out, Inherit)
                .spawn()?,
        );
    }
    for c in &cs {
        codes.push(c.process.wait()?.to_string());
    }
    println(&format!("codes: [{}]", codes.join(", ")))?;
    println(&catch(
        cmd("printf").args(&["\\377"]).output(None),
        |o| format!("bad stdout?? {}", o.exit_code),
        |e| format!("bad stdout: {e}"),
    ))
}

fn output_big(_: &[String]) -> R<()> {
    let o = cmd("sh")
        .args(&[
            "-c",
            "yes b 2>/dev/null | head -c 200000 >&2; yes a 2>/dev/null | head -c 300000; exit 4",
        ])
        .output(None)?;
    println(&format!(
        "code {} stdout {} stderr {}",
        o.exit_code,
        o.stdout.chars().count(),
        o.stderr.chars().count()
    ))?;
    let o = cmd("sh").args(&["-c", "printf '\\377' >&2"]).output(None)?;
    println(&format!("code {}", o.exit_code))
}

/// The case `process/output_oom` (under `ulimit -v`): a child that writes one
/// pipe without end and has closed the other.
fn output_oom(args: &[String]) -> R<()> {
    let which = args.first().map(String::as_str).unwrap_or("");
    println(&format!("before {which}"))?;
    let script = if which == "stdout" {
        "exec yes 2>&-"
    } else {
        "exec yes >&2"
    };
    // the crate's stopping sink: the glue ends with Lean's out of memory
    // once `output` returns (AR-5)
    let (mut o, mut e) = (StoppingSink::default(), StoppingSink::default());
    let r = cmd("sh")
        .args(&["-c", script])
        .with(|a| process::output(a, None, &mut o, &mut e));
    let (o, e) = match (o.finish(), e.finish()) {
        (Ok(o), Ok(e)) => (o, e),
        (Err(oom), _) | (_, Err(oom)) => internal_panic(oom.into()),
    };
    let code = r?;
    println(&format!(
        "after {which}: exit {code}, {} and {} bytes",
        o.len(),
        e.len()
    ))
}

/// The case `process/output_drain_exit`: `output` fails on standard error
/// while the child still writes standard output; the process waits for that
/// pipe's end after `main` (`finish`).
fn output_drain_exit(_: &[String]) -> R<()> {
    drain_then_main_ends()
}

/// `output_drain_exit`'s `try ... catch` and its last line.
fn drain_then_main_ends() -> R<()> {
    let r = cmd("sh")
        .args(&[
            "-c",
            "printf '\\377' >&2; exec 2>&-; sleep 1; echo late; echo \"child: write status $?\" > marker",
        ])
        .output(None);
    match r {
        Ok(o) => println(&format!("output: exit {}", o.exit_code))?,
        Err(e) => println(&format!("output failed: {}", to_string(&e)))?,
    }
    println("main ends")
}

/// The case `process/output_drain_oom` (under `ulimit -v`): `output` fails
/// on standard error while `yes` writes standard output; the drain keeps
/// the bytes until its storage cannot grow, then ends the process with the
/// out-of-memory internal panic while `finish` waits for it (RFX1-04).
fn output_drain_oom(_: &[String]) -> R<()> {
    let r = cmd("sh")
        .args(&["-c", "printf '\\377' >&2; exec 2>&-; exec yes"])
        .output(None);
    match r {
        Ok(o) => println(&format!("output: exit {}", o.exit_code))?,
        Err(e) => println(&format!("output failed: {}", to_string(&e)))?,
    }
    println("main ends")
}

/// The case `process/output_drain_exit_exit`: `IO.Process.exit 0` while the
/// drain still reads; the exit does not wait for it (LB-29).
fn output_drain_exit_exit(_: &[String]) -> R<()> {
    drain_then_main_ends()?;
    exit::exit(0)
}

/// The case `process/output_drain_exit_panic`: an internal panic
/// (`Array.replicate` of the size in argv, 2^64) while the drain still
/// reads; the panic's exit does not wait for it (LB-29).
fn output_drain_exit_panic(args: &[String]) -> R<()> {
    drain_then_main_ends()?;
    let n: Option<u64> = args[0].parse().ok();
    let len = array::replicate_len(n).unwrap_or_else(|p| internal_panic(p));
    println(&len.to_string())
}

/// The case `process/output_drain_exit_force`: `IO.Process.forceExit 0`
/// after flushing standard output; nothing waits for the drain, as natively.
fn output_drain_exit_force(_: &[String]) -> R<()> {
    drain_then_main_ends()?;
    stdout().flush()?;
    exit::force_exit(0)
}

/// The case `process/output_oom_both_pipes` (under `ulimit -v`): `output` of
/// `yes`, whose standard error stays open; the panic ends the process at
/// once (LB-29: natively it hangs).
fn output_oom_both_pipes(args: &[String]) -> R<()> {
    println("before")?;
    let (mut o, mut e) = (StoppingSink::default(), StoppingSink::default());
    let r = cmd(args.first().map(String::as_str).unwrap_or("yes"))
        .with(|a| process::output(a, None, &mut o, &mut e));
    let (o, _) = match (o.finish(), e.finish()) {
        (Ok(o), Ok(e)) => (o, e),
        (Err(oom), _) | (_, Err(oom)) => internal_panic(oom.into()),
    };
    let code = r?;
    println(&format!("after {code} {}", o.len()))
}

fn failed_rows(label: &str, a: Spawn) -> R<()> {
    let c = a.stdio(Null, Null, Piped).spawn()?;
    println(&format!(
        "{label}: stderr {}",
        quote(&read_to_end(&c.stderr)?)
    ))?;
    lenv::sleep(100);
    let p = &c.process;
    println(&catch(
        p.kill(),
        |()| format!("{label}: kill before wait ok"),
        |e| format!("{label}: kill before wait: {e}"),
    ))?;
    println(&format!("{label}: wait {}", p.wait()?))?;
    println(&catch(
        p.kill(),
        |()| format!("{label}: kill after wait ok"),
        |e| format!("{label}: kill after wait: {e}"),
    ))?;
    println(&catch(
        p.wait(),
        |w| format!("{label}: second wait {w}"),
        |e| format!("{label}: second wait: {e}"),
    ))?;
    println(&catch(
        p.try_wait(),
        |t| format!("{label}: tryWait {}", opt(t)),
        |e| format!("{label}: tryWait: {e}"),
    ))
}

fn failed_child_rows(args: &[String]) -> R<()> {
    let c = args.first().map_or("no-such-program-xyz", String::as_str);
    failed_rows("program", cmd(c))?;
    failed_rows("program setsid", cmd(c).setsid(true))?;
    failed_rows("cwd", cmd("true").cwd("no-such-dir"))?;
    failed_rows("cwd setsid", cmd("true").cwd("no-such-dir").setsid(true))?;
    let ch = cmd(c).stdio(Null, Null, Null).spawn()?;
    lenv::sleep(100);
    println(&format!("tryWait first {}", opt(ch.process.try_wait()?)))?;
    println(&catch(
        ch.process.wait(),
        |w| format!("wait after tryWait {w}"),
        |e| format!("wait after tryWait: {e}"),
    ))
}

fn failed_child_pid(args: &[String]) -> R<()> {
    let c = args.first().map_or("no-such-program-xyz", String::as_str);
    let ch = cmd(c).stdio(Null, Null, Null).spawn()?;
    println(&format!("program: pid positive {}", ch.process.pid() > 0))?;
    println(&format!("program: wait {}", ch.process.wait()?))?;
    let ch = cmd("true")
        .cwd("no-such-dir")
        .stdio(Null, Null, Null)
        .spawn()?;
    println(&format!("cwd: pid positive {}", ch.process.pid() > 0))?;
    println(&format!("cwd: wait {}", ch.process.wait()?))
}

fn attempt(label: &str, a: Spawn) -> R<()> {
    match a.output(None) {
        Ok(o) => report_flush(label, Ok(o)),
        Err(e) => println(&format!("{label}: error {}", to_string(&e))),
    }
}

fn cwd_nosearch(_: &[String]) -> R<()> {
    let top = cwd();
    let here = format!("{top}/here");
    lfs::create_dir(here.as_bytes())?;
    lfs::create_dir(format!("{here}/sub").as_bytes())?;
    lfs::set_current_dir(here.as_bytes())?;
    set_rights(&here, true, true, false)?;
    attempt("relative", cmd("pwd").cwd("sub"))?;
    attempt("absolute", cmd("pwd").cwd("/"))?;
    attempt(
        "absolute missing program",
        cmd("no-such-program-xyz").cwd("/"),
    )?;
    attempt("none", cmd("sh").args(&["-c", "echo ran"]))?;
    set_rights(&here, true, true, true)?;
    println(&format!("cwd unchanged: {}", cwd() == here))?;
    lfs::set_current_dir(top.as_bytes())?;
    lfs::remove_dir(format!("{here}/sub").as_bytes())?;
    lfs::remove_dir(here.as_bytes())
}

fn take_stdin_setsid(_: &[String]) -> R<()> {
    for take in [false, true] {
        let c = cmd("sh")
            .args(&["-c", "sleep 30 & echo $!; cat >/dev/null; wait"])
            .stdio(Piped, Piped, Inherit)
            .setsid(true)
            .spawn()?;
        let line = get_line(&c.stdout)?;
        let gc = line.trim().to_owned();
        let code = if take {
            let p = c.process.take_stdin();
            drop(c.stdin);
            p.kill()?;
            p.wait()?
        } else {
            c.process.kill()?;
            c.process.wait()?
        };
        let chk = cmd("sh").args(&["-c", &gone_probe(&gc)]).output(None)?;
        println(&format!(
            "takeStdin {take}: killed {code}, grandchild {}",
            chk.stdout.trim()
        ))?;
        cmd("sh")
            .args(&["-c", &format!("kill -9 {gc} 2>/dev/null; true")])
            .output(None)?;
    }
    Ok(())
}

fn exec_report(label: &str, a: Spawn) -> R<()> {
    let line = catch(
        a.output(None),
        |o| {
            format!(
                "{label}: exit {} out {} err {}",
                o.exit_code,
                quote(&o.stdout),
                quote(&o.stderr)
            )
        },
        |e| format!("{label}: error {e}"),
    );
    println(&line)?;
    stdout().flush()
}

fn exec_search(_: &[String]) -> R<()> {
    for d in ["bin1", "bin2", "work", "work/bin3"] {
        lfs::create_dir(d.as_bytes())?;
    }
    write_file("bin1/tool", "echo from bin1\n")?;
    set_rights("bin1/tool", true, true, false)?;
    write_file("bin2/tool", "echo \"from bin2 $# $*\"\n")?;
    set_rights("bin2/tool", true, true, true)?;
    write_file("work/bin3/other", "echo from bin3\n")?;
    set_rights("work/bin3/other", true, true, true)?;
    write_file("plain", "x")?;
    let top = cwd();
    exec_report("empty name", cmd(""))?;
    exec_report(
        "eacces then found",
        cmd("tool")
            .args(&["a", "b"])
            .env(&[("PATH", Some(&format!("{top}/bin1:{top}/bin2")))]),
    )?;
    exec_report(
        "eacces only",
        cmd("tool").env(&[("PATH", Some(&format!("{top}/bin1")))]),
    )?;
    exec_report(
        "enotdir entry",
        cmd("tool").env(&[("PATH", Some(&format!("{top}/plain:{top}/bin2")))]),
    )?;
    exec_report(
        "empty entry",
        cmd("tool")
            .env(&[("PATH", Some(":/nonexistent"))])
            .cwd("bin2"),
    )?;
    exec_report(
        "relative entry after cwd",
        cmd("other").env(&[("PATH", Some("bin3"))]).cwd("work"),
    )?;
    exec_report(
        "empty PATH",
        cmd("tool").env(&[("PATH", Some(""))]).cwd("bin2"),
    )?;
    exec_report(
        "no PATH",
        cmd("sh")
            .args(&["-c", "echo default path"])
            .env(&[("PATH", None)]),
    )?;
    exec_report("slash relative after cwd", cmd("./bin3/other").cwd("work"))?;
    exec_report("directory", cmd("/tmp"))?;
    exec_report("not executable with slash", cmd("bin1/tool"))?;
    exec_report("long name", cmd(&"n".repeat(300)))?;
    exec_report("nul in cmd", cmd("sh\0junk").args(&["-c", "echo nul cmd"]))?;
    exec_report(
        "nul in cwd",
        cmd("sh")
            .args(&["-c", "basename \"$PWD\""])
            .cwd("work\0junk"),
    )?;
    exec_report("nul in bad cwd", cmd("pwd").cwd("missing\0junk"))?;
    exec_report(
        "nul in env name",
        cmd("sh")
            .args(&["-c", "echo \"[${AB-unset}][${A-unset}]\""])
            .env(&[("A\0B", Some("v"))]),
    )
}

// ---- temp ----

/// `describe` of the case: the path's length less 6, then its start.
fn describe(p: &str) -> String {
    let n = p.chars().count();
    let start: String = p.chars().take(n - 6).collect();
    format!("{} chars then 6: {}", n - 6, quote(&start))
}

fn temp_file() -> R<(Handle, String)> {
    let mut p = Vec::new();
    let h = temp::create_temp_file(&mut p)?;
    Ok((h, String::from_utf8_lossy(&p).into_owned()))
}

fn temp_dir() -> R<String> {
    let mut p = Vec::new();
    temp::create_temp_dir(&mut p)?;
    Ok(String::from_utf8_lossy(&p).into_owned())
}

fn remove_dir_all(p: &str) -> R<()> {
    let mut names = Vec::new();
    lfs::read_dir(p.as_bytes(), |n| {
        names.push(String::from_utf8_lossy(n).into_owned())
    })?;
    for n in names {
        let full = format!("{p}/{n}");
        if lfs::symlink_metadata(full.as_bytes())?.file_type == lfs::FileType::Dir {
            remove_dir_all(&full)?;
        } else {
            lfs::remove_file(full.as_bytes())?;
        }
    }
    lfs::remove_dir(p.as_bytes())
}

fn temp_files(args: &[String]) -> R<()> {
    let body = || -> R<()> {
        let (h, path) = temp_file()?;
        println(&format!("temp file: {}", describe(&path)))?;
        let md = lfs::metadata(path.as_bytes())?;
        println(&format!("size {} type IO.FS.FileType.file", md.byte_size))?;
        h.put_str(b"hello temp\nsecond\n")?;
        h.flush()?;
        h.rewind()?;
        let mut l = Vec::new();
        h.get_line(&mut l)?;
        println(&format!(
            "read back: {}",
            quote(&String::from_utf8_lossy(&l))
        ))?;
        println(&format!("via readFile: {}", quote(&read_file(&path)?)))?;
        lfs::remove_file(path.as_bytes())?;
        println(&format!(
            "exists after remove: {}",
            lfs::metadata(path.as_bytes()).is_ok()
        ))?;
        let dir = temp_dir()?;
        println(&format!("temp dir: {}", describe(&dir)))?;
        println(&format!(
            "is dir: {}",
            lfs::metadata(dir.as_bytes())
                .map(|m| m.file_type == lfs::FileType::Dir)
                .unwrap_or(false)
        ))?;
        write_file(&format!("{dir}/inner.txt"), "inside")?;
        println(&format!(
            "inner: {}",
            read_file(&format!("{dir}/inner.txt"))?
        ))?;
        remove_dir_all(&dir)?;
        println(&format!(
            "dir exists after remove: {}",
            lfs::metadata(dir.as_bytes()).is_ok()
        ))?;
        // withTempFile: the file is removed after the action, the handle dropped first
        let (h, p) = temp_file()?;
        h.put_str(b"scoped\n")?;
        h.flush()?;
        let r = read_file(&p)?;
        drop(h);
        lfs::remove_file(p.as_bytes())?;
        println(&format!("withTempFile: {}", quote(&r)))?;
        let d = temp_dir()?;
        write_file(&format!("{d}/x"), "y")?;
        let mut n = 0;
        lfs::read_dir(d.as_bytes(), |_| n += 1)?;
        remove_dir_all(&d)?;
        println(&format!("withTempDir entries: {n}"))
    };
    if let Err(e) = body() {
        println(&format!("error: {}", to_string(&e)))?;
    }
    if args == ["dir-only"] {
        match temp_dir() {
            Ok(d) => {
                println(&format!("second dir: {}", describe(&d)))?;
                lfs::remove_dir(d.as_bytes())?;
            }
            Err(e) => println(&format!("dir error: {}", to_string(&e)))?,
        }
    }
    Ok(())
}

fn temp_modes(_: &[String]) -> R<()> {
    let mode_of = |p: &str| -> R<String> {
        Ok(cmd("stat")
            .args(&["-c", "%a", p])
            .output(None)?
            .stdout
            .trim()
            .to_owned())
    };
    let (h, p) = temp_file()?;
    println(&format!("file mode {}", mode_of(&p)?))?;
    h.put_str(b"abc")?;
    h.rewind()?;
    let mut b = [0u8; 10];
    let n = h.read(&mut b)?;
    let shown: Vec<String> = b[..n].iter().map(|x| x.to_string()).collect();
    println(&format!("read back [{}]", shown.join(", ")))?;
    let o = cmd("sh")
        .args(&["-c", "ls -l /proc/$$/fd | grep -c tmp. || true"])
        .output(None)?;
    println(&format!("child sees the file: {}", o.stdout.trim()))?;
    lfs::remove_file(p.as_bytes())?;
    let d = temp_dir()?;
    println(&format!("dir mode {}", mode_of(&d)?))?;
    lfs::remove_dir(d.as_bytes())
}

fn temp_missing_dir(_: &[String]) -> R<()> {
    println("before")?;
    println(&catch(
        temp_file(),
        |(_, p)| format!("created {p}"),
        |e| format!("file: {e}"),
    ))?;
    println(&catch(
        temp_dir(),
        |d| format!("created {d}"),
        |e| format!("dir: {e}"),
    ))?;
    println("after")
}

/// `describe` of the case `io/temp_file_error`: `noFileOrDirectory` with
/// its fields, any other error as its text.
fn describe_error(e: &IoError) -> String {
    match e {
        IoError::NoFileOrDirectory(f, c, m) => {
            format!("noFileOrDirectory {} {c} {}", quote(f), quote(m))
        }
        e => format!("other error: {}", to_string(e)),
    }
}

/// LB-03's case `io/temp_file_error` (`TMPDIR` names a missing directory):
/// the correct outcome, which the case expects.
fn temp_file_error(args: &[String]) -> R<()> {
    println(&format!("before {}", args.len()))?;
    match temp_file() {
        Ok((_, p)) => println(&format!("temp file {p}"))?,
        Err(e) => println(&describe_error(&e))?,
    }
    match temp_dir() {
        Ok(p) => println(&format!("temp dir {p}"))?,
        Err(e) => println(&describe_error(&e))?,
    }
    println("after")
}

fn temp_long_dir(_: &[String]) -> R<()> {
    println("before")?;
    stdout().flush()?;
    println(&catch(
        temp_dir(),
        |d| format!("created {d}"),
        |e| format!("dir: {e}"),
    ))?;
    println("after")
}

// ---- uvsys ----

fn bytes_to(f: impl FnOnce(&mut Vec<u8>) -> R<()>) -> R<String> {
    let mut v = Vec::new();
    f(&mut v)?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

fn try_p(label: &str, r: R<String>) -> R<()> {
    println(&match r {
        Ok(s) => format!("{label}: {s}"),
        Err(e) => format!("{label}: error {}", to_string(&e)),
    })
}

fn getenv(name: &str) -> Option<String> {
    let mut v = Vec::new();
    uvsys::os_getenv(name.as_bytes(), &mut v).then(|| String::from_utf8_lossy(&v).into_owned())
}

fn io_getenv(name: &str) -> Option<String> {
    let mut v = Vec::new();
    lenv::get_env(name.as_bytes(), &mut v).then(|| String::from_utf8_lossy(&v).into_owned())
}

/// `toString` of an `Option String`: `(some (a b))` when the value holds a
/// space, as Lean's `ToString (Option α)` parenthesizes.
fn opt_s(o: Option<String>) -> String {
    match o {
        Some(s) if s.contains(' ') => format!("(some ({s}))"),
        Some(s) => format!("(some {s})"),
        None => "none".to_owned(),
    }
}

fn environ_with(prefix: &str) -> String {
    let mut v = Vec::new();
    uvsys::os_environ(|k, val| {
        let k = String::from_utf8_lossy(k);
        if k.starts_with(prefix) {
            v.push(format!("({k}, {})", String::from_utf8_lossy(val)));
        }
    });
    format!("[{}]", v.join(", "))
}

fn uv_system(_: &[String]) -> R<()> {
    try_p(
        "title",
        bytes_to(uvsys::get_process_title).map(|t| t.ends_with("uv_system").to_string()),
    )?;
    try_p(
        "pid",
        Ok((uvsys::os_getpid() == u64::from(lenv::get_pid())).to_string()),
    )?;
    try_p("ppid", Ok((uvsys::os_getppid() > 0).to_string()))?;
    try_p(
        "cwd",
        bytes_to(uvsys::cwd).map(|c| (c == cwd()).to_string()),
    )?;
    try_p(
        "exePath",
        bytes_to(uvsys::exepath).and_then(|e| Ok((e == bytes_to(lenv::app_path)?).to_string())),
    )?;
    try_p(
        "chdir missing",
        uvsys::chdir(b"no-such-dir").map(|()| "()".into()),
    )?;
    try_p("chdir nul", uvsys::chdir(b"a\0b").map(|()| "()".into()))?;
    write_file("plain.txt", "x")?;
    try_p(
        "chdir file",
        uvsys::chdir(b"plain.txt").map(|()| "()".into()),
    )?;
    lfs::create_dir(b"sub")?;
    try_p(
        "chdir sub",
        uvsys::chdir(b"sub")
            .and_then(|()| bytes_to(uvsys::cwd))
            .map(|c| c.ends_with("/sub").to_string()),
    )?;
    try_p(
        "chdir back",
        uvsys::chdir(b"..")
            .and_then(|()| bytes_to(uvsys::cwd))
            .map(|c| (c == cwd()).to_string()),
    )?;
    lfs::remove_file(b"plain.txt")?;
    lfs::remove_dir(b"sub")?;
    try_p(
        "homedir",
        bytes_to(uvsys::os_homedir).map(|h| (Some(h) == io_getenv("HOME")).to_string()),
    )?;
    try_p("tmpdir", bytes_to(uvsys::os_tmpdir))?;
    try_p(
        "passwd",
        uvsys::os_get_passwd().map(|p| {
            format!(
                "{} {} {} {} {}",
                !p.username.is_empty(),
                p.uid.is_some(),
                p.gid.is_some(),
                p.shell.is_some(),
                p.homedir.is_some()
            )
        }),
    )?;
    try_p("environ", Ok(environ_with("IOFIX")))?;
    try_p(
        "getenv",
        Ok(format!(
            "{} {} {} {}",
            opt_s(getenv("IOFIX_A")),
            opt_s(getenv("IOFIX_NONE")),
            opt_s(getenv("a\0b")),
            opt_s(getenv(""))
        )),
    )?;
    try_p(
        "setenv",
        uvsys::os_setenv(b"IOFIX_SET", b"by uv").map(|()| {
            format!(
                "{} {}",
                opt_s(getenv("IOFIX_SET")),
                opt_s(io_getenv("IOFIX_SET"))
            )
        }),
    )?;
    try_p(
        "setenv overwrite",
        uvsys::os_setenv(b"IOFIX_A", b"again").map(|()| opt_s(getenv("IOFIX_A"))),
    )?;
    try_p(
        "setenv =",
        uvsys::os_setenv(b"A=B", b"x").map(|()| "()".into()),
    )?;
    try_p(
        "setenv empty",
        uvsys::os_setenv(b"", b"x").map(|()| "()".into()),
    )?;
    try_p(
        "setenv nul",
        uvsys::os_setenv(b"A\0", b"x").map(|()| "()".into()),
    )?;
    try_p(
        "setenv nul value",
        uvsys::os_setenv(b"A", b"x\0y").map(|()| "()".into()),
    )?;
    try_p(
        "unsetenv",
        uvsys::os_unsetenv(b"IOFIX_SET").map(|()| opt_s(getenv("IOFIX_SET"))),
    )?;
    try_p(
        "unsetenv =",
        uvsys::os_unsetenv(b"A=B").map(|()| "()".into()),
    )?;
    try_p(
        "unsetenv nul",
        uvsys::os_unsetenv(b"A\0").map(|()| "()".into()),
    )?;
    try_p(
        "unsetenv missing",
        uvsys::os_unsetenv(b"IOFIX_NEVER_SET").map(|()| "()".into()),
    )?;
    try_p("environ after", Ok(environ_with("IOFIX")))?;
    try_p(
        "hostname",
        bytes_to(uvsys::os_gethostname).and_then(|h| {
            let k = read_file("/proc/sys/kernel/hostname")?;
            Ok((h == k[..k.len() - 1]).to_string())
        }),
    )?;
    try_p(
        "uname",
        uvsys::os_uname().and_then(|u| {
            let rel = read_file("/proc/sys/kernel/osrelease")?;
            Ok(format!(
                "{} {} {} {}",
                String::from_utf8_lossy(&u.sysname),
                u.release == rel.as_bytes()[..rel.len() - 1],
                !u.version.is_empty(),
                !u.machine.is_empty()
            ))
        }),
    )?;
    try_p("uptime", uvsys::uptime().map(|u| (u > 0).to_string()))?;
    try_p("hrtime", {
        let a = uvsys::hrtime();
        lenv::sleep(2);
        let b = uvsys::hrtime();
        Ok((a > 0 && b >= a + 2_000_000).to_string())
    })?;
    try_p(
        "rusage",
        uvsys::getrusage().map(|r| {
            format!(
                "{} {} {} {} {} {} {} {}",
                r.max_rss > 0,
                r.ix_rss,
                r.id_rss,
                r.is_rss,
                r.n_swap,
                r.msg_sent,
                r.msg_recv,
                r.signals
            )
        }),
    )?;
    try_p("memory", {
        let (free, total, avail, c) = (
            uvsys::free_memory(),
            uvsys::total_memory(),
            uvsys::available_memory(),
            uvsys::constrained_memory(),
        );
        Ok(format!(
            "{} {} {} {}",
            free > 0,
            total > free,
            avail > 0,
            c == 0 || avail <= c || avail <= free
        ))
    })?;
    try_p(
        "priority self",
        uvsys::os_getpriority(0)
            .and_then(|a| Ok((a == uvsys::os_getpriority(uvsys::os_getpid())?).to_string())),
    )?;
    try_p(
        "priority 2^32",
        uvsys::os_getpriority(4294967296)
            .and_then(|a| Ok((a == uvsys::os_getpriority(0)?).to_string())),
    )?;
    try_p(
        "priority missing",
        uvsys::os_getpriority(2147483647).map(|p| p.to_string()),
    )?;
    try_p(
        "priority negative pid",
        uvsys::os_getpriority(4294967295).map(|p| p.to_string()),
    )?;
    try_p(
        "set priority same",
        uvsys::os_getpriority(0)
            .and_then(|p| uvsys::os_setpriority(0, p))
            .map(|()| "()".into()),
    )?;
    try_p(
        "set priority 20",
        uvsys::os_setpriority(0, 20).map(|()| "()".into()),
    )?;
    try_p(
        "set priority -21",
        uvsys::os_setpriority(0, -21).map(|()| "()".into()),
    )?;
    try_p(
        "set priority missing",
        uvsys::os_setpriority(2147483647, 1).map(|()| "()".into()),
    )?;
    try_p(
        "set priority negative pid",
        uvsys::os_setpriority(4294967295, 1).map(|()| "()".into()),
    )?;
    try_p(
        "set priority 2^32+19",
        uvsys::os_setpriority(0, 4294967315)
            .and_then(|()| uvsys::os_getpriority(0))
            .map(|p| p.to_string()),
    )?;
    try_p(
        "cpus",
        uvsys::cpu_info().and_then(|c| {
            let stat = read_file("/proc/stat")?;
            let n = stat
                .split('\n')
                .skip(1)
                .take_while(|l| l.starts_with("cpu"))
                .count();
            Ok(format!(
                "{} {} {}",
                c.len() == n,
                c.iter().all(|x| !x.model.is_empty()),
                c.iter().all(|x| x.times.user % 10 == 0)
            ))
        }),
    )
}

fn try_io(label: &str, r: R<String>) -> R<()> {
    println(&match r {
        Ok(s) => format!("{label}: {s}"),
        Err(e) => format!("{label}: error: {}", to_string(&e)),
    })
}

fn brief(s: &str) -> String {
    let start: String = s.chars().take(12).collect();
    format!("{} {}", s.len(), quote(&start))
}

fn uv_limits(args: &[String]) -> R<()> {
    try_io("osHomedir", bytes_to(uvsys::os_homedir).map(|s| brief(&s)))?;
    try_io("osTmpdir", bytes_to(uvsys::os_tmpdir).map(|s| brief(&s)))?;
    if args == ["env"] {
        return Ok(());
    }
    let p = uvsys::os_getpriority(0)?;
    for q in [20i64, -21, 100, -100, 4294967296 + 25, 4294967296 * 5 - 30] {
        try_io(
            &format!("setPriority {q}"),
            uvsys::os_setpriority(0, q).map(|()| "ok".into()),
        )?;
    }
    println(&format!(
        "priority unchanged {}",
        uvsys::os_getpriority(0)? == p
    ))?;
    for n in [600, 511, 512] {
        try_io(
            &format!("title of {n}"),
            uvsys::set_process_title("T".repeat(n).as_bytes())
                .and_then(|()| bytes_to(uvsys::get_process_title))
                .map(|t| t.len().to_string()),
        )?;
    }
    try_io(
        "random 0x80000000",
        uvsys::random_check(0x8000_0000).map(|()| "started".into()),
    )
}

fn quoted_title(s: &str) -> String {
    quote(&s.replace('\0', "|"))
}

fn process_title(args: &[String]) -> R<()> {
    let t = bytes_to(uvsys::get_process_title)?;
    println(&format!(
        "initial: {}",
        if args == ["title"] {
            t
        } else {
            t.ends_with("process_title").to_string()
        }
    ))?;
    let comm = || read_file("/proc/thread-self/comm");
    uvsys::set_process_title(b"short")?;
    println(&format!(
        "after set: {} thread name {}",
        quoted_title(&bytes_to(uvsys::get_process_title)?),
        quoted_title(&comm()?)
    ))?;
    uvsys::set_process_title(b"a-title-longer-than-sixteen-bytes-and-more-and-more-and-more")?;
    println(&format!(
        "after long: {} thread name {}",
        quoted_title(&bytes_to(uvsys::get_process_title)?),
        quoted_title(&comm()?)
    ))?;
    if let Err(e) = uvsys::set_process_title(b"nul\0inside") {
        println(&format!("nul: {}", to_string(&e)))?;
    }
    println(&format!(
        "after nul: {}",
        quoted_title(&bytes_to(uvsys::get_process_title)?)
    ))?;
    uvsys::set_process_title(b"")?;
    println(&format!(
        "empty: {} thread name {}",
        quoted_title(&bytes_to(uvsys::get_process_title)?),
        quoted_title(&comm()?)
    ))
}

/// Lean's `lean_mk_string` of bytes (the crate's `lossy_utf8`).
fn lean_lossy(b: &[u8]) -> String {
    let mut out = String::new();
    let _ = lean_runtime::semantics::string::lossy_utf8(b, &mut out);
    out
}

fn info(s: &str) -> String {
    format!(
        "{} ({} bytes, {} characters)",
        quote(s),
        s.len(),
        s.chars().count()
    )
}

fn os_strings_lossy(args: &[String]) -> R<()> {
    let raw = |f: &dyn Fn(&mut Vec<u8>) -> R<()>| -> R<String> {
        let mut v = Vec::new();
        f(&mut v)?;
        Ok(lean_lossy(&v))
    };
    match args.first().map(String::as_str) {
        Some("title") => {
            uvsys::set_process_title("üüüüü".as_bytes())?;
            println(&format!(
                "title {}",
                info(&raw(&|v| uvsys::get_process_title(v))?)
            ))
        }
        Some("temp") => {
            let mut p = Vec::new();
            let h = temp::create_temp_file(&mut p)?;
            h.put_str(b"x")?;
            let p = lean_lossy(&p);
            let (parent, name) = p.rsplit_once('/').unwrap();
            let parent_name = parent.rsplit('/').next().unwrap();
            println(&format!(
                "createTempFile in {}, name of {}, exists {}",
                info(parent_name),
                name.chars().count(),
                lfs::metadata(p.as_bytes()).is_ok()
            ))?;
            let mut d = Vec::new();
            temp::create_temp_dir(&mut d)?;
            let d = lean_lossy(&d);
            let (parent, name) = d.rsplit_once('/').unwrap();
            let parent_name = parent.rsplit('/').next().unwrap();
            println(&format!(
                "createTempDir in {}, name of {}, exists {}",
                info(parent_name),
                name.chars().count(),
                lfs::metadata(d.as_bytes()).is_ok()
            ))
        }
        _ => {
            let g = {
                let mut v = Vec::new();
                uvsys::os_getenv(b"L2R_BAD", &mut v).then(|| lean_lossy(&v))
            };
            println(&format!(
                "osGetenv {}",
                g.map_or("none".into(), |s| info(&s))
            ))?;
            let mut found = None;
            uvsys::os_environ(|k, v| {
                if found.is_none() && lean_lossy(k) == "L2R_BAD" {
                    found = Some(lean_lossy(v));
                }
            });
            println(&format!(
                "osEnviron {}",
                found.map_or("none".into(), |s| info(&s))
            ))?;
            let e = {
                let mut v = Vec::new();
                lenv::get_env(b"L2R_BAD", &mut v).then(|| lean_lossy(&v))
            };
            println(&format!(
                "IO.getEnv {}",
                e.map_or("none".into(), |s| info(&s))
            ))?;
            println(&format!(
                "osHomedir {}",
                info(&raw(&|v| uvsys::os_homedir(v))?)
            ))?;
            println(&format!(
                "osTmpdir {}",
                info(&raw(&|v| uvsys::os_tmpdir(v))?)
            ))
        }
    }
}

// ---- streams ----

fn text(b: &Rc<RefCell<Vec<u8>>>) -> String {
    String::from_utf8(b.borrow().clone()).unwrap()
}

fn rt_streams_redirect_oob(_: &[String]) -> R<()> {
    let buf: Rc<RefCell<Vec<u8>>> = Rc::default();
    let old = set_stderr(Stream::Buffer(buf.clone()));
    // `xs[5 + k]!` out of bounds: the runtime's panic, then the default 0
    debug::runtime_eprintln(b"Error: index out of bounds");
    println("oob 0")?;
    set_stderr(old);
    println(&format!("captured stderr: {}", quote(&text(&buf))))
}

fn task_streams(_: &[String]) -> R<()> {
    let out: Rc<RefCell<Vec<u8>>> = Rc::default();
    let err: Rc<RefCell<Vec<u8>>> = Rc::default();
    let old_out = set_stdout(Stream::Buffer(out.clone()));
    let old_err = set_stderr(Stream::Buffer(err.clone()));
    println("main: captured")?;
    eprintln("main: captured error")?;
    // `IO.asTask` runs on a worker thread, which starts with the process's streams
    let r = std::thread::spawn(|| -> Result<String, String> {
        let body = || -> R<String> {
            println("task: to the real stdout")?;
            eprintln("task: to the real stderr")?;
            let mine: Rc<RefCell<Vec<u8>>> = Rc::default();
            let prev = set_stdout(Stream::Buffer(mine.clone()));
            println("task: own buffer")?;
            set_stdout(prev);
            stdout().flush()?;
            Ok(text(&mine))
        };
        body().map_err(|e| to_string(&e))
    })
    .join()
    .unwrap();
    println("main: still captured")?;
    set_stdout(old_out);
    set_stderr(old_err);
    // `repr r.toOption`
    println(&format!(
        "task result: {}",
        r.ok()
            .map_or("none".into(), |s| format!("some {}", quote(&s)))
    ))?;
    println(&format!("main stdout buffer: {}", quote(&text(&out))))?;
    println(&format!("main stderr buffer: {}", quote(&text(&err))))?;
    std::thread::spawn(|| {
        let _ = println("dedicated task: real stdout");
        let _ = stdout().flush();
    })
    .join()
    .unwrap();
    println("end")
}

// ---- time ----

/// `IO.FS.withIsolatedStreams` (Lean code): standard input, output and (with
/// `isolate_stderr`) error set to buffers around `x`, restored after; the
/// output buffer's text and `x`'s result.
fn with_isolated<T>(isolate_stderr: bool, x: impl FnOnce() -> R<T>) -> R<(String, T)> {
    let out: Rc<RefCell<Vec<u8>>> = Rc::default();
    let old_in = streams::set_stdin(Stream::Buffer(Rc::default()), || {
        Stream::Handle(Handle::stdin())
    });
    let old_out = set_stdout(Stream::Buffer(out.clone()));
    let old_err = isolate_stderr.then(|| set_stderr(Stream::Buffer(out.clone())));
    let r = x();
    if let Some(e) = old_err {
        set_stderr(e);
    }
    set_stdout(old_out);
    streams::set_stdin(old_in, || Stream::Handle(Handle::stdin()));
    Ok((text(&out), r?))
}

fn timeit<T>(msg: &str, act: impl FnOnce() -> R<T>) -> R<T> {
    lean_runtime::io::time::timeit(msg.as_bytes(), act)
}

fn shape(line: &str) -> String {
    let parts: Vec<&str> = line.split(' ').collect();
    match parts.as_slice() {
        [msg, num] => {
            let unit = if num.ends_with("ms") {
                "ms"
            } else if num.ends_with('s') {
                "s"
            } else {
                "?"
            };
            format!("{msg} <{unit}>")
        }
        _ => format!("other: {line}"),
    }
}

fn timeit_shapes(args: &[String]) -> R<()> {
    let n = args.len() as u64;
    let via_param = |act: &dyn Fn() -> R<u64>| timeit("param", act);
    let (err, r) = with_isolated(true, || {
        let act = || Ok(7 + n);
        let a = timeit("twice-a", act)?;
        let b = timeit("twice-b", act)?;
        let c = via_param(&|| Ok(9 + n))?;
        let d = timeit("from-array", || Ok(2 + n))?;
        let e = timeit("once", || {
            eprintln("inside once")?;
            Ok(5 + n)
        })?;
        let f = timeit("ref-param", || via_param(&act))?;
        let g = timeit("outer", || {
            let x = timeit("inner", || {
                eprintln("inside")?;
                Ok(1)
            })?;
            let y = timeit("inner2", || Ok(2))?;
            Ok(x + y)
        })?;
        Ok(format!("{a} {b} {c} {d} {e} {f} {g}"))
    })?;
    println(&r)?;
    for l in err.split('\n').filter(|l| !l.is_empty()) {
        println(&shape(l))?;
    }
    Ok(())
}

fn shape_ok(line: &str, msg: &str) -> bool {
    let Some(rest) = line.strip_prefix(&format!("{msg} ")) else {
        return false;
    };
    let rest = rest.strip_suffix('\n').unwrap_or(rest);
    let num = rest
        .strip_suffix("ms")
        .or_else(|| rest.strip_suffix('s'))
        .unwrap_or("x");
    let mantissa = num.split('e').next().unwrap_or("");
    let significant: String = mantissa
        .chars()
        .filter(char::is_ascii_digit)
        .skip_while(|&c| c == '0')
        .collect();
    !num.is_empty()
        && num
            .chars()
            .all(|c| c.is_ascii_digit() || ".e-+".contains(c))
        && significant.len() <= 3
}

fn timeit_child(args: &[String]) -> R<()> {
    if args == ["child"] {
        let r = timeit("label one", || Ok(41))?;
        println(&format!("child result {}", r + 1))?;
        let r = timeit("label two", || {
            lenv::sleep(20);
            Ok("slept")
        })?;
        println(&format!("child result {r}"))?;
        if let Err(e) = timeit::<u64>("label three", || Err(IoError::user_error("inside"))) {
            println(&format!("child caught {}", to_string(&e)))?;
        }
        // `main` returns 5
        let _ = stdout().flush();
        exit::exit(5);
    }
    let me = bytes_to(lenv::app_path)?;
    let o = cmd(&me).args(&["child"]).output(None)?;
    println(&format!("child exit {}", o.exit_code))?;
    println(&format!("child stdout {}", quote(&o.stdout)))?;
    let lines: Vec<&str> = o.stderr.split('\n').filter(|l| !l.is_empty()).collect();
    println(&format!("stderr lines {}", lines.len()))?;
    for (l, m) in lines.iter().zip(["label one", "label two", "label three"]) {
        println(&format!("{m}: {}", shape_ok(l, m)))?;
    }
    Ok(())
}

// ---- more uvsys ----

fn cwd_long(_: &[String]) -> R<()> {
    let try_io2 = |label: &str, r: R<String>| {
        println(&match r {
            Ok(s) => format!("  {label}: {s}"),
            Err(e) => format!("  {label}: error: {}", to_string(&e)),
        })
    };
    let report = |len: usize| -> R<()> {
        println(&format!("cwd of {len} bytes"))?;
        try_io2(
            "IO.currentDir",
            bytes_to(lfs::current_dir).map(|s| s.len().to_string()),
        )?;
        try_io2(
            "IO.Process.getCurrentDir",
            bytes_to(lfs::process_current_dir).map(|s| s.len().to_string()),
        )?;
        try_io2(
            "realPath .",
            bytes_to(|v| lfs::real_path(b".", v)).map(|s| s.len().to_string()),
        )?;
        try_io2("uv cwd", bytes_to(uvsys::cwd).map(|s| s.len().to_string()))
    };
    let enter = |n: usize| -> R<()> {
        let d = "d".repeat(n);
        lfs::create_dir(d.as_bytes())?;
        lfs::set_current_dir(d.as_bytes())
    };
    let home = cwd();
    let top = format!("{home}/rtcwdlong");
    lfs::create_dir(top.as_bytes())?;
    lfs::set_current_dir(top.as_bytes())?;
    let mut len = top.len();
    while 4095 - len > 251 {
        enter(200)?;
        len += 201;
    }
    let k = 4095 - len - 1;
    enter(k)?;
    report(4095)?;
    lfs::set_current_dir(b"..")?;
    enter(k + 1)?;
    report(4096)?;
    enter(200)?;
    report(4297)?;
    lfs::set_current_dir(home.as_bytes())?;
    let _ = cmd("rm").args(&["-rf", &top]).output(None)?;
    println(&format!(
        "removed {}",
        lfs::metadata(top.as_bytes()).is_err()
    ))
}

fn rt_system(_: &[String]) -> R<()> {
    let s = |f: &dyn Fn(&mut Vec<u8>) -> R<()>| -> R<String> {
        let mut v = Vec::new();
        f(&mut v)?;
        Ok(String::from_utf8_lossy(&v).into_owned())
    };
    println(&format!(
        "pid {}",
        uvsys::os_getpid() == u64::from(lenv::get_pid())
    ))?;
    println(&format!("ppid positive {}", uvsys::os_getppid() > 0))?;
    println(&format!("cwd {}", s(&|v| uvsys::cwd(v))? == cwd()))?;
    println(&format!(
        "home {}",
        s(&|v| uvsys::os_homedir(v))? == io_getenv("HOME").unwrap_or_default()
    ))?;
    println(&format!("tmpdir {}", s(&|v| uvsys::os_tmpdir(v))?))?;
    let u = uvsys::os_uname()?;
    println(&format!(
        "uname {} {} release nonempty {}",
        String::from_utf8_lossy(&u.sysname),
        String::from_utf8_lossy(&u.machine),
        !u.release.is_empty()
    ))?;
    println(&format!("hostname {}", s(&|v| uvsys::os_gethostname(v))?))?;
    let pw = uvsys::os_get_passwd()?;
    let home = s(&|v| uvsys::os_homedir(v))?;
    println(&format!(
        "passwd {} uid {} home {}",
        String::from_utf8_lossy(&pw.username),
        pw.uid.is_some(),
        pw.homedir.as_deref() == Some(home.as_bytes())
    ))?;
    if let Some(g) = pw.gid {
        println(&format!(
            "group {}",
            uvsys::os_get_group(g)?.map(|x| x.gid) == Some(g)
        ))?;
    }
    println(&format!(
        "no group {}",
        uvsys::os_get_group(4000000)?.is_none()
    ))?;
    uvsys::os_setenv(b"L2R_SYS_TEST", b"value 1")?;
    println(&format!(
        "getenv {} {}",
        opt_s(getenv("L2R_SYS_TEST")),
        opt_s(io_getenv("L2R_SYS_TEST"))
    ))?;
    let mut has = false;
    uvsys::os_environ(|k, v| has |= k == b"L2R_SYS_TEST" && v == b"value 1");
    println(&format!("environ has it {has}"))?;
    uvsys::os_unsetenv(b"L2R_SYS_TEST")?;
    println(&format!("after unset {}", opt_s(getenv("L2R_SYS_TEST"))))?;
    if let Err(e) = uvsys::os_setenv(b"", b"x") {
        println(&format!("empty name: {}", to_string(&e)))?;
    }
    if let Err(e) = uvsys::os_setenv(b"A\0B", b"x") {
        println(&format!("NUL: {}", to_string(&e)))?;
    }
    let p = uvsys::os_getpriority(0)?;
    uvsys::os_setpriority(0, p)?;
    println(&format!("priority {p} {}", uvsys::os_getpriority(0)? == p))?;
    let (t1, t2) = (uvsys::hrtime(), uvsys::hrtime());
    println(&format!("hrtime monotonic {}", t2 >= t1))?;
    uvsys::random_check(16)?;
    let mut b = [0u8; 16];
    match uvsys::random_fill(&mut b) {
        Ok(()) => println(&format!("random {}", b.len()))?,
        Err(e) => println(&format!("random failed {}", to_string(&e)))?,
    }
    println(&format!(
        "rusage maxRSS positive {}",
        uvsys::getrusage()?.max_rss > 0
    ))?;
    println(&format!(
        "exePath {}",
        s(&|v| uvsys::exepath(v))? == bytes_to(lenv::app_path)?
    ))?;
    let total = uvsys::total_memory();
    println(&format!(
        "memory {} {} {}",
        total > 0,
        uvsys::free_memory() <= total,
        uvsys::available_memory() <= total
    ))?;
    let cpus = uvsys::cpu_info()?;
    let mut models: Vec<String> = Vec::new();
    for c in &cpus {
        let m = String::from_utf8_lossy(&c.model).into_owned();
        if !models.contains(&m) {
            models.push(m);
        }
    }
    println(&format!(
        "cpus {} models [{}]",
        cpus.len(),
        models.join(", ")
    ))?;
    println(&format!(
        "constrained memory {}",
        uvsys::constrained_memory()
    ))?;
    println(&format!("uptime positive {}", uvsys::uptime()? > 0))?;
    uvsys::set_process_title(b"ab")?;
    println(&format!("title {}", s(&|v| uvsys::get_process_title(v))?))?;
    uvsys::chdir(b"/")?;
    println(&format!("cwd after chdir {}", s(&|v| uvsys::cwd(v))?))?;
    if let Err(e) = uvsys::chdir(b"/no/such/dir") {
        println(&format!("chdir: {}", to_string(&e)))?;
    }
    Ok(())
}

fn uv_queries(_: &[String]) -> R<()> {
    let non_empty =
        |label: &str, r: R<String>| try_p(label, r.map(|s| (!s.is_empty()).to_string()));
    non_empty("hostname", bytes_to(uvsys::os_gethostname))?;
    non_empty("tmpdir", bytes_to(uvsys::os_tmpdir))?;
    try_p("total", Ok((uvsys::total_memory() > 0).to_string()))?;
    try_p("pid", Ok((uvsys::os_getpid() > 0).to_string()))?;
    try_p("total again", Ok((uvsys::total_memory() > 0).to_string()))?;
    try_p(
        "total once more",
        Ok((uvsys::total_memory() > 0).to_string()),
    )?;
    let p = uvsys::os_get_passwd()?;
    let h = bytes_to(uvsys::os_homedir)?.into_bytes();
    println(&format!(
        "{} {} {}",
        Some(&h) == p.homedir.as_ref(),
        true,
        true
    ))?;
    // p2 := { p with homedir := some h, shell := p.homedir }
    println(&format!(
        "{} {} {}",
        Some(&h) == p.homedir.as_ref(),
        true,
        true
    ))?;
    // q : PasswdInfo := ⟨"u", some 1, none, some h, p.shell⟩: homedir is p.shell
    println(&format!("u {}", p.shell.as_ref() == Some(&h)))?;
    let r = uvsys::getrusage()?;
    // bumpR r := { r with maxRSS := 0, signals := 7 }
    let r2 = uvsys::RUsage {
        max_rss: 0,
        signals: 7,
        ..r
    };
    println(&format!(
        "{} {} {}",
        r2.max_rss,
        r2.signals,
        r2.user_time == r.user_time
    ))
}

// ---- more streams ----

fn boom(label: &str, n: u64) -> u64 {
    if n > 100 {
        n
    } else {
        debug::runtime_eprintln(format!("PANIC at boom {label}: boom").as_bytes());
        0
    }
}

fn stream_redirect(_: &[String]) -> R<()> {
    println("start")?;
    let buf: Rc<RefCell<Vec<u8>>> = Rc::default();
    let old = set_stdout(Stream::Buffer(buf.clone()));
    println("captured one")?;
    print("captured two")?;
    old.put_str(b"through the previous stream\n")?;
    let mine = set_stdout(old);
    mine.put_str(b" (and through the returned buffer stream)\n")?;
    println(&format!("buffer: {}", quote(&text(&buf))))?;
    let ebuf: Rc<RefCell<Vec<u8>>> = Rc::default();
    let olderr = set_stderr(Stream::Buffer(ebuf.clone()));
    eprintln("error captured")?;
    set_stderr(olderr);
    eprintln("error to the real stream")?;
    println(&format!("stderr buffer: {}", quote(&text(&ebuf))))?;
    // standard input from a buffer: `IO.FS.Stream.ofBuffer`'s getLine
    let data = b"first line\nsecond line\nrest".to_vec();
    let mut pos = 0;
    let mut lines = Vec::new();
    for _ in 0..4 {
        let rest = &data[pos..];
        let n = rest
            .iter()
            .position(|&c| c == b'\n')
            .map_or(rest.len(), |k| k + 1);
        lines.push(quote(&String::from_utf8_lossy(&rest[..n])));
        pos += n;
    }
    println(&format!("stdin lines: {}", lines.join(" ")))?;
    let real = get_line(&Some(Handle::stdin()))?;
    println(&format!("real stdin: {}", quote(&real)))?;
    let b1: Rc<RefCell<Vec<u8>>> = Rc::default();
    let b2: Rc<RefCell<Vec<u8>>> = Rc::default();
    let s0 = set_stdout(Stream::Buffer(b1.clone()));
    println("to b1")?;
    let s1 = set_stdout(Stream::Buffer(b2.clone()));
    println("to b2")?;
    set_stdout(s1);
    println("to b1 again")?;
    set_stdout(s0);
    println(&format!(
        "b1: {} b2: {}",
        quote(&text(&b1)),
        quote(&text(&b2))
    ))?;
    let (out, r) = with_isolated(true, || {
        println("isolated out")?;
        eprintln("isolated err")?;
        Ok(42)
    })?;
    println(&format!("isolated: {} {r}", quote(&out)))?;
    let (out2, ()) = with_isolated(false, || {
        println("isolated out only")?;
        eprintln("not isolated err")
    })?;
    println(&format!("isolated stdout only: {}", quote(&out2)))?;
    let (perr, v) = with_isolated(true, || {
        debug::runtime_eprintln(b"Error: index out of bounds");
        let b = boom("M:8:51", 3);
        println(&format!("got 0 and {b}"))?;
        Ok(7)
    })?;
    println(&format!("isolated panics: {} {v}", quote(&perr)))?;
    let tbuf: Rc<RefCell<Vec<u8>>> = Rc::default();
    let olderr2 = set_stderr(Stream::Buffer(tbuf.clone()));
    let t = timeit("timed", || Ok(3))?;
    println(&format!("panic under setStderr: {}", boom("M:8:51", 4)))?;
    set_stderr(olderr2);
    let ttext = text(&tbuf);
    let tlines: Vec<&str> = ttext.split('\n').collect();
    let first = tlines.first().copied().unwrap_or("");
    println(&format!(
        "timeit captured: {} {} {} {} {t}",
        tlines.len(),
        first.starts_with("timed "),
        first.ends_with("ms"),
        quote(tlines.get(1).copied().unwrap_or(""))
    ))?;
    println(&format!("after restore: {}", boom("M:8:51", 5)))?;
    println("end")
}

fn rt_streams_redirect(args: &[String]) -> R<()> {
    let k = args.len() as u64;
    let buf: Rc<RefCell<Vec<u8>>> = Rc::default();
    let old = set_stderr(Stream::Buffer(buf.clone()));
    let b = if 3 + k > 2 {
        debug::runtime_eprintln(format!("PANIC at boom M:6:42: boom {}", 3 + k).as_bytes());
        0
    } else {
        3 + k
    };
    println(&format!("panic {b}"))?;
    debug::dbg_trace(format!("traced {k}").as_bytes());
    println(&format!("trace {}", k + 1))?;
    let r = debug::allocprof(b"profiled", || k + 7);
    println(&format!("allocprof {r}"))?;
    eprintln("explicit eprintln")?;
    set_stderr(old);
    println(&format!("captured stderr:\n{}", text(&buf)))?;
    eprintln("back on real stderr")?;
    // `xs[7 + k]!` on the real stderr
    debug::runtime_eprintln(b"Error: index out of bounds");
    println("after 0")
}

// ---- disagreement cases ----

fn null_fd_leak(_: &[String]) -> R<()> {
    let script = "n=0; for f in /proc/$$/fd/*; do i=${f##*/}; if [ \"$i\" -gt 2 ] && [ \"$(readlink $f)\" = /dev/null ]; then n=$((n+1)); fi; done; echo $n >&2";
    for (label, i, o) in [
        ("stdin and stdout null", Null, Null),
        ("stdout null", Piped, Null),
        ("none null", Piped, Piped),
    ] {
        let c = cmd("sh").args(&["-c", script]).stdio(i, o, Piped).spawn()?;
        let r = read_to_end(&c.stderr)?;
        c.process.wait()?;
        println(&format!("{label}: {}", r.trim()))?;
    }
    Ok(())
}

/// LIO2-06: the title is written over the arguments' memory, which the
/// crate's constructor kept (`io::argv_title`).
fn title_cmdline(args: &[String]) -> R<()> {
    let before = read_file("/proc/self/cmdline")?;
    let environ_before = read_file("/proc/self/environ")?;
    println(&format!(
        "cmdline before holds the argument: {}",
        before.contains("abcdefghijklmnop")
    ))?;
    uvsys::set_process_title(b"new-title")?;
    println(&format!("title: {}", bytes_to(uvsys::get_process_title)?))?;
    let c = read_file("/proc/self/cmdline")?;
    println(&format!(
        "cmdline starts with the title: {}",
        c.starts_with("new-title")
    ))?;
    println(&format!(
        "then only NUL bytes: {}, as long as before: {}",
        c.chars().skip(9).all(|ch| ch == '\0'),
        c.chars().count() == before.chars().count()
    ))?;
    uvsys::set_process_title("z".repeat(before.chars().count() + 5).as_bytes())?;
    let t = bytes_to(uvsys::get_process_title)?;
    println(&format!(
        "a longer title is cut to the memory less one byte: {}",
        t.chars().count() + 1 == before.chars().count()
    ))?;
    let c2 = read_file("/proc/self/cmdline")?;
    println(&format!(
        "cmdline is the cut title and one NUL: {}",
        c2 == format!("{t}\0")
    ))?;
    println(&format!(
        "environment kept: {}",
        opt(io_getenv("TITLE_ENV"))
    ))?;
    println(&format!(
        "environment's memory unchanged: {}",
        read_file("/proc/self/environ")? == environ_before
    ))?;
    println(&format!("args: [{}]", args.join(", ")))
}

/// A module initializer sets a long title; `main`'s `args` are then built
/// from the arguments, as a translator builds them after the initializers
/// (`std::env::args`, which reads the table the crate's constructor pointed
/// at copies of the arguments).
fn title_in_initializer(_: &[String]) -> R<()> {
    uvsys::set_process_title("é".repeat(300).as_bytes())?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    println(&format!("args: [{}]", args.join(", ")))?;
    // `IO.FS.readBinFile`: `read` until empty (the file's size is 0)
    let h = Handle::open(b"/proc/self/cmdline", FsMode::Read)?;
    let mut c = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = h.read(&mut buf)?;
        if n == 0 {
            break;
        }
        c.extend_from_slice(&buf[..n]);
    }
    println(&format!(
        "cmdline starts with the title: {}",
        c.starts_with(&[0xC3, 0xA9, 0xC3, 0xA9])
    ))
}

/// LQ1-01: started through the dynamic loader, the crate keeps no
/// arguments' memory, so the title fails with `ENOBUFS` (native writes it).
fn title_via_loader(args: &[String]) -> R<()> {
    match uvsys::set_process_title(b"loader-title") {
        Ok(()) => println("title set")?,
        Err(e) => println(&format!("title not set: {}", to_string(&e)))?,
    }
    let c = read_file("/proc/self/cmdline")?;
    println(&format!(
        "cmdline holds the title: {}",
        c.contains("loader-title")
    ))?;
    println(&format!("args: [{}]", args.join(", ")))
}

/// A754: `readDir`'s entry, its name replaced; `DirEntry.path` is
/// `root / fileName`.
fn dir_entry_update(_: &[String]) -> R<()> {
    lfs::create_dir(b"d")?;
    write_file("d/a.txt", "x")?;
    let mut names = Vec::new();
    lfs::read_dir(b"d", |n| {
        names.push(String::from_utf8_lossy(n).into_owned())
    })?;
    match names.first() {
        Some(_) => println("renamed.lean true renamed.lean root-lit/name-lit")?,
        None => println("none")?,
    }
    lfs::remove_file(b"d/a.txt")?;
    lfs::remove_dir(b"d")
}

// ---- the modelled errno (review RIO2-02) ----

/// The case's `probe`: a read handle whose sticky error flag a failed write
/// sets, `act`, then `getLine`, which reports the modelled `errno`.
fn errno_probe(label: &str, act: impl FnOnce() -> R<()>) -> R<()> {
    write_file("e.txt", "line1\n")?;
    let h = Handle::open(b"e.txt", FsMode::Read)?;
    let _ = h.put_str(b"x").and_then(|()| h.flush());
    if let Err(e) = act() {
        println(&format!("{label} act: {}", to_string(&e)))?;
    }
    match get_line(&Some(h)) {
        Ok(l) => println(&format!("{label}: ok {}", quote(&l))),
        Err(e) => println(&format!("{label}: {}", to_string(&e))),
    }
}

/// The case's `deepDir`: enter a directory chain whose path is `len` bytes.
fn deep_dir(len: usize) -> R<String> {
    let top = format!("{}/deep", cwd());
    lfs::create_dir(top.as_bytes())?;
    lfs::set_current_dir(top.as_bytes())?;
    let mut cur = top.len();
    while len - cur > 251 {
        let d = "d".repeat(200);
        lfs::create_dir(d.as_bytes())?;
        lfs::set_current_dir(d.as_bytes())?;
        cur += 201;
    }
    let d = "d".repeat(len - cur - 1);
    lfs::create_dir(d.as_bytes())?;
    lfs::set_current_dir(d.as_bytes())?;
    Ok(top)
}

fn errno_after(args: &[String]) -> R<()> {
    let ignore = |r: R<()>| r;
    match args.first().map(String::as_str) {
        Some("home") => {
            return errno_probe("osHomedir without HOME", || {
                bytes_to(uvsys::os_homedir).map(drop)
            })
        }
        Some("with-home") => {
            return errno_probe("osHomedir with HOME", || {
                bytes_to(uvsys::os_homedir).map(drop)
            })
        }
        Some("tmp") => {
            errno_probe("createTempFile tmpdir error", || temp_file().map(drop))?;
            return errno_probe("osTmpdir error", || bytes_to(uvsys::os_tmpdir).map(drop));
        }
        _ => {}
    }
    let made: RefCell<Vec<(String, bool)>> = RefCell::new(Vec::new());
    errno_probe("baseline", || Ok(()))?;
    errno_probe("createTempFile", || {
        let (_, p) = temp_file()?;
        made.borrow_mut().push((p, false));
        Ok(())
    })?;
    errno_probe("createTempDir", || {
        let p = temp_dir()?;
        made.borrow_mut().push((p, true));
        Ok(())
    })?;
    errno_probe("osGetPriority", || uvsys::os_getpriority(0).map(drop))?;
    errno_probe("osGetPasswd", || uvsys::os_get_passwd().map(drop))?;
    errno_probe("osGetGroup", || uvsys::os_get_group(0).map(drop))?;
    errno_probe("osTmpdir", || bytes_to(uvsys::os_tmpdir).map(drop))?;
    errno_probe("osSetPriority 20", || uvsys::os_setpriority(0, 20))?;
    errno_probe("random big", || uvsys::random_check(0x8000_0000))?;
    errno_probe("osGetPriority negative pid", || {
        uvsys::os_getpriority(4294967295).map(drop)
    })?;
    errno_probe("spawn and wait", || {
        cmd("true").spawn().and_then(|c| c.process.wait()).map(drop)
    })?;
    errno_probe("output", || cmd("true").output(None).map(drop))?;
    errno_probe("cpuInfo", || ignore(uvsys::cpu_info().map(drop)))?;
    for (p, dir) in made.take() {
        if dir {
            lfs::remove_dir(p.as_bytes())?;
        } else {
            lfs::remove_file(p.as_bytes())?;
        }
    }
    for len in [4096, 4297] {
        let home = cwd();
        let top = deep_dir(len)?;
        errno_probe(&format!("cwd of {len} bytes"), || {
            bytes_to(uvsys::cwd).map(drop)
        })?;
        lfs::set_current_dir(home.as_bytes())?;
        cmd("rm").args(&["-rf", &top]).output(None)?;
    }
    Ok(())
}

/// The case `memory_exact` (review RIO2-09).
fn memory_exact(_: &[String]) -> R<()> {
    let read_limit = |p: &str| -> u64 {
        match read_file(p) {
            Ok(t) if t.trim() == "max" => u64::MAX,
            Ok(t) => t.trim().parse().unwrap_or(0),
            Err(_) => 0,
        }
    };
    let meminfo = read_file("/proc/meminfo")?;
    let total: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|r| r.trim().split(' ').next()?.parse::<u64>().ok())
        .map_or(0, |k| k * 1024);
    println(&format!(
        "total exact: {} {}",
        uvsys::total_memory() == total,
        total > 0
    ))?;
    let cg = read_file("/proc/self/cgroup")?;
    if let Some(rest) = cg.strip_prefix("0::/") {
        let dir = format!("/sys/fs/cgroup/{}", rest.split('\n').next().unwrap_or(""));
        let (max, high) = (
            read_limit(&format!("{dir}/memory.max")),
            read_limit(&format!("{dir}/memory.high")),
        );
        let want = if max == 0 || high == 0 {
            0
        } else {
            max.min(high)
        };
        println(&format!(
            "constrained exact: {}",
            uvsys::constrained_memory() == want
        ))?;
        let avail = uvsys::available_memory();
        let bound = if want == 0 || want > total {
            total
        } else {
            want
        };
        println(&format!(
            "available bounded: {} {}",
            avail <= bound,
            avail > 0
        ))?;
    } else {
        println("constrained exact: true")?;
        println("available bounded: true true")?;
    }
    let stat = read_file("/proc/stat")?;
    let n = stat
        .split('\n')
        .skip(1)
        .take_while(|l| l.starts_with("cpu"))
        .count();
    println(&format!(
        "cpus: {} {}",
        uvsys::cpu_info()?.len() == n,
        n > 0
    ))
}

// ---- Lean-bug cases (LB-16, LB-17) ----

/// The cases `temp_long_file`, `temp_long_dir_4095` and `temp_long_bounds`:
/// `createTempFile` or `createTempDir` under the
/// `.pipe`'s `TMPDIR`.
fn temp_long(args: &[String]) -> R<()> {
    println("before")?;
    stdout().flush()?;
    let r = if args == ["dir"] {
        temp_dir().and_then(|d| {
            println(&format!("created dir ({} bytes)", d.chars().count()))?;
            lfs::remove_dir(d.as_bytes())
        })
    } else {
        temp_file().and_then(|(_, p)| {
            println(&format!("created file ({} bytes)", p.chars().count()))?;
            lfs::remove_file(p.as_bytes())
        })
    };
    if let Err(e) = r {
        println(&format!("caught: {}", to_string(&e)))?;
    }
    println("after")
}

/// The case `null_open_fails` (LB-17): descriptors exhausted, then `null`
/// streams.
fn null_open_fails(_: &[String]) -> R<()> {
    let mut hs = Vec::new();
    while let Ok(h) = Handle::open(b"/dev/null", FsMode::Read) {
        hs.push(h);
    }
    let attempt = |label: &str, a: Spawn| -> R<()> {
        println(label)?;
        stdout().flush()?;
        match a.spawn() {
            Ok(c) => println(&format!("  exit {}", c.process.wait()?)),
            Err(e) => println(&format!("  spawn failed: {}", to_string(&e))),
        }
    };
    attempt(
        "stdout null:",
        cmd("sh")
            .args(&["-c", "echo '  child: written to the parent'"])
            .stdio(Inherit, Null, Inherit),
    )?;
    attempt(
        "stdin null:",
        cmd("sh")
            .args(&[
                "-c",
                "if read x; then echo \"  child: read $x\"; else echo '  child: no input'; fi",
            ])
            .stdio(Null, Inherit, Inherit),
    )?;
    let l = get_line(&Some(Handle::stdin()))?;
    println(&format!("parent reads: {}", l.trim()))?;
    println(&format!("handles kept open: {}", !hs.is_empty()))
}

/// The case `pipe_null_two_free` (LB-17's cost): two free descriptors, a
/// piped standard output and a `null` standard error.
fn pipe_null_two_free(_: &[String]) -> R<()> {
    let mut hs = Vec::new();
    while let Ok(h) = Handle::open(b"/dev/null", FsMode::Read) {
        hs.push(h);
    }
    hs.pop();
    hs.pop();
    match cmd("sh")
        .args(&["-c", "echo out; echo err >&2"])
        .stdio(Inherit, Piped, Null)
        .spawn()
    {
        Ok(c) => {
            let out = read_to_end(&c.stdout)?;
            println(&format!(
                "child: stdout {out:?}, exit {}",
                c.process.wait()?
            ))?;
        }
        Err(e) => println(&format!("spawn failed: {}", to_string(&e)))?,
    }
    println(&format!("handles kept open: {}", !hs.is_empty()))
}

// ---- leanrs review cases (LRIO2-F2, F3, LB-03 through osGetGroup) ----

fn failed_child_order(args: &[String]) -> R<()> {
    print("pending ")?;
    let c = cmd(args.first().map_or("no-such-program-xyz", String::as_str))
        .stdio(Null, Inherit, Inherit)
        .spawn()?;
    eprintln("parent after spawn")?;
    let code = c.process.wait()?;
    eprintln(&format!("waited {code}"))
}

fn spawn_fds_exhausted(_: &[String]) -> R<()> {
    let mut hs = Vec::new();
    while let Ok(h) = Handle::open(b"/dev/null", FsMode::Read) {
        hs.push(h);
    }
    let attempt = |label: &str, a: Spawn| -> R<()> {
        match a.spawn() {
            Ok(c) => println(&format!("{label}: exit {}", c.process.wait()?))?,
            Err(e) => println(&format!("{label}: spawn failed: {}", to_string(&e)))?,
        }
        stdout().flush()
    };
    attempt("missing program", cmd("no-such-program-xyz"))?;
    attempt("relative cwd", cmd("true").cwd("."))?;
    attempt("absolute cwd", cmd("true").cwd("/"))?;
    attempt("missing dir", cmd("true").cwd("no-such-dir"))?;
    println(&format!("kept {}", !hs.is_empty()))
}

fn group_missing_errno(args: &[String]) -> R<()> {
    let gid: u64 = args.first().map_or(3999999, |a| a.parse().unwrap());
    println("before")?;
    stdout().flush()?;
    write_file("e.txt", "line1\n")?;
    let h = Handle::open(b"e.txt", FsMode::Read)?;
    let _ = h.put_str(b"x").and_then(|()| h.flush());
    let g = uvsys::os_get_group(gid)?;
    println(&format!("group none {}", g.is_none()))?;
    match get_line(&Some(h)) {
        Ok(l) => println(&format!("getLine: ok {}", quote(&l)))?,
        Err(e) => println(&format!("getLine: {}", to_string(&e)))?,
    }
    println("after")
}

// ---- debug, clock and allocator cases (cases-xt) ----

/// `@[noinline] def traced (n : Nat) : Nat := dbgTrace s!"trace {n}" fun _ => n + 1`.
fn traced(n: u64) -> u64 {
    debug::dbg_trace(format!("trace {n}").as_bytes());
    n + 1
}

/// The case `debug/dbg_trace_current_stderr` (DV5): the trace goes to the
/// current standard error, a buffer inside `IO.withStderr`.
fn dbg_trace_current_stderr(args: &[String]) -> R<()> {
    let n = args.len() as u64;
    eprintln("before")?;
    let buf: Rc<RefCell<Vec<u8>>> = Rc::default();
    // `IO.withStderr`: set, run, restore (also on an error)
    let old = set_stderr(Stream::Buffer(buf.clone()));
    let r = (|| -> R<u64> {
        let size = buf.borrow().len() as u64;
        let v = traced(n + size);
        println(&format!("value {v}"))?;
        Ok(v)
    })();
    set_stderr(old);
    let v = r?;
    println(&format!("captured {}", quote(&text(&buf))))?;
    let w = traced(v + 1);
    eprintln(&format!("after {w}"))
}

/// The case `debug/dbg_sleep` (DV5): `dbgSleep 300` sleeps between the two
/// clock readings.
fn dbg_sleep(args: &[String]) -> R<()> {
    let n = args.len() as u64;
    eprintln("before sleep")?;
    let t0 = lenv::mono_ms_now();
    // `@[noinline] def slept (ms : UInt32) (n : Nat) : Nat := dbgSleep ms fun _ => n + 1`
    debug::dbg_sleep(300);
    let v = n + 1;
    println(&format!("value {v}"))?;
    let t1 = lenv::mono_ms_now();
    eprintln("after sleep")?;
    println(&format!(
        "slept at least 300 ms: {}",
        t1.saturating_sub(t0) >= 300
    ))
}

/// The case `debug/dbg_stack_trace_continues` (DV5): `dbgStackTrace`
/// continues without a trace, so the glue calls nothing (the case's
/// `normalize` drops native's frame lines).
fn dbg_stack_trace_continues(args: &[String]) -> R<()> {
    let n = args.len() as u64;
    eprintln("before")?;
    println(&format!("value {}", n + 1))?;
    eprintln("after")
}

/// The case `clock/mono_clock_origin` (DV10): `IO.monoNanosNow` and
/// `IO.monoMsNow` count from the boot, as `/proc/uptime` does.
fn mono_clock_origin(args: &[String]) -> R<()> {
    let s = read_file("/proc/uptime")?;
    let up: u64 = s.split('.').next().unwrap_or("").parse().unwrap_or(0);
    let near = |a: u64, b: u64| a.abs_diff(b) <= 2;
    let ns = lenv::mono_nanos_now();
    let ms = lenv::mono_ms_now();
    println(&format!(
        "monoNanosNow is the uptime: {}",
        near(ns / 1_000_000_000, up)
    ))?;
    println(&format!("monoMsNow is the uptime: {}", near(ms / 1000, up)))?;
    println(&format!("arguments {}", args.len()))
}

/// `lean_internal_panic`: the crate's executor (`io::panic::internal_panic`,
/// native's glue): its line on the C `stderr`, then `exit(1)` (or an abort
/// under `LEAN_ABORT_ON_PANIC`).
fn internal_panic(p: InternalPanic) -> ! {
    lean_runtime::io::panic::internal_panic(p.message(), &mut lean_runtime::io::panic::Native)
}

/// The case `panics/replicate_overflow`: the allocators' size rules
/// (`semantics::array`); the size is below 2^64 (`Nat::to_u64`) or `None`.
/// A size the rules accept is reserved (the translator's allocator; its
/// failure is `out of memory` too); the case's sizes never get there.
fn replicate_overflow(args: &[String]) -> R<()> {
    let n: Option<u64> = args[1].parse().ok();
    let word = array::WORD_ELEMENT_BYTES;
    let reserve = |elem: u64, len: usize| {
        let mut v: Vec<u8> = Vec::new();
        if v.try_reserve_exact(len.saturating_mul(elem as usize))
            .is_err()
        {
            internal_panic(InternalPanic::OutOfMemory);
        }
    };
    let size = match args[0].as_str() {
        "replicate" | "replicateNat" | "replicateInt" | "replicateFloat" => {
            let len = array::replicate_len(n).unwrap_or_else(|p| internal_panic(p));
            reserve(word, len);
            len
        }
        k @ ("mkEmpty" | "mkEmptyNat" | "byteArray" | "floatArray") => {
            let elem = if k == "byteArray" {
                array::BYTE_ELEMENT_BYTES
            } else {
                word
            };
            let c = array::empty_with_capacity(elem, n.unwrap_or(u64::MAX))
                .unwrap_or_else(|p| internal_panic(p));
            reserve(elem, c);
            0
        }
        _ => return println("unknown case"),
    };
    println(&size.to_string())
}

// ---- the twins ----

type Twin = fn(&[String]) -> R<()>;

const TWINS: &[(&str, Twin)] = &[
    ("proc_spawn", proc_spawn),
    ("proc_output", proc_output),
    ("proc_inherit", proc_inherit),
    ("failed_child_stdin", failed_child_stdin),
    ("output_fields", output_fields),
    ("rt_process", rt_process),
    ("rt_process_spawn", rt_process_spawn),
    ("output_big", output_big),
    ("output_oom", output_oom),
    ("output_drain_exit", output_drain_exit),
    ("output_drain_exit_exit", output_drain_exit_exit),
    ("output_drain_exit_panic", output_drain_exit_panic),
    ("output_drain_exit_force", output_drain_exit_force),
    ("output_oom_both_pipes", output_oom_both_pipes),
    ("output_drain_oom", output_drain_oom),
    // `closed_stdout` has no twin: the twin's Rust entry (`lang_start`) opens
    // `/dev/null` on a closed standard descriptor before `main` (leanrs DV19),
    // which a translator's entry does not do
    ("failed_child_rows", failed_child_rows),
    ("failed_child_pid", failed_child_pid),
    ("cwd_nosearch", cwd_nosearch),
    ("take_stdin_setsid", take_stdin_setsid),
    ("exec_search", exec_search),
    ("temp_files", temp_files),
    ("temp_modes", temp_modes),
    ("temp_missing_dir", temp_missing_dir),
    ("temp_long_dir", temp_long_dir),
    ("temp_file_error", temp_file_error),
    ("uv_system", uv_system),
    ("uv_limits", uv_limits),
    ("process_title", process_title),
    ("os_strings_lossy", os_strings_lossy),
    ("rt_streams_redirect_oob", rt_streams_redirect_oob),
    ("task_streams", task_streams),
    ("stream_redirect", stream_redirect),
    ("rt_streams_redirect", rt_streams_redirect),
    ("timeit_shapes", timeit_shapes),
    ("timeit_child", timeit_child),
    ("cwd_long", cwd_long),
    ("rt_system", rt_system),
    ("uv_queries", uv_queries),
    ("null_fd_leak", null_fd_leak),
    ("title_cmdline", title_cmdline),
    ("title_in_initializer", title_in_initializer),
    ("title_via_loader", title_via_loader),
    ("dir_entry_update", dir_entry_update),
    ("errno_after", errno_after),
    ("memory_exact", memory_exact),
    ("temp_long_file", temp_long),
    ("temp_long_dir_4095", temp_long),
    ("temp_long_bounds", temp_long),
    ("null_open_fails", null_open_fails),
    ("pipe_null_two_free", pipe_null_two_free),
    ("failed_child_order", failed_child_order),
    ("spawn_fds_exhausted", spawn_fds_exhausted),
    ("group_missing_errno", group_missing_errno),
    ("dbg_trace_current_stderr", dbg_trace_current_stderr),
    ("dbg_sleep", dbg_sleep),
    ("dbg_stack_trace_continues", dbg_stack_trace_continues),
    ("mono_clock_origin", mono_clock_origin),
    ("replicate_overflow", replicate_overflow),
];

/// The twins of the cases that set the process title and expect native's
/// outcome, where the title is written: checked only with the feature
/// `proc-title` (without it, `setProcessTitle` fails with `ENOBUFS`).
const NEED_PROC_TITLE: &[&str] = &[
    "uv_limits",
    "process_title",
    "os_strings_lossy",
    "rt_system",
    "title_cmdline",
    "title_in_initializer",
];

fn main() {
    if cfg!(miri) {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    // the twin named by the binary's file name, or by `argv[0]`'s when the
    // binary was started through the dynamic loader (`ld.so ./title_via_loader`),
    // whose path `current_exe` then gives
    let argv0 = std::env::args().next().unwrap_or_default();
    let argv0_name = argv0.rsplit('/').next().unwrap_or_default().to_owned();
    if let Some((_, twin)) = TWINS
        .iter()
        .find(|(n, _)| *n == name)
        .or_else(|| TWINS.iter().find(|(n, _)| *n == argv0_name))
    {
        // with `startup-fds`, a glue's call at `main`'s start (the crate's
        // constructor opened native's startup descriptors; this keeps it
        // linked)
        #[cfg(feature = "startup-fds")]
        lean_runtime::io::startup::ensure_native_descriptors();
        let args: Vec<String> = std::env::args().skip(1).collect();
        // a threads build runs the twin inside a task (`in_task`)
        #[cfg(feature = "threads")]
        finish(in_task::run(*twin, args));
        #[cfg(not(feature = "threads"))]
        finish(twin(&args));
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let ids: Vec<&str> = TWINS
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| cfg!(feature = "proc-title") || !NEED_PROC_TITLE.contains(id))
        .collect();
    if !cfg!(feature = "proc-title") {
        println!(
            "io2_cases: without the feature proc-title, not checked: {}",
            NEED_PROC_TITLE.join(", ")
        );
    }
    // hard links next to the binary (same file system), one per case
    let dir = exe
        .parent()
        .unwrap()
        .join(format!("io2-twins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for id in &ids {
        let w = dir.join(id);
        if std::fs::hard_link(&exe, &w).is_err() {
            std::fs::copy(&exe, &w).unwrap();
        }
    }
    let status = std::process::Command::new("python3")
        .arg(format!("{root}/scripts/cases.py"))
        .args(["check", "--exe-dir"])
        .arg(&dir)
        .args(&ids)
        .env("LEAN_RUNTIME_NO_CAP", "1")
        .status()
        .expect("python3 scripts/cases.py");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        status.success(),
        "a twin differs from native Lean (see above)"
    );
}
