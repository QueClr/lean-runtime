//! Lean's IO definitions the sched-io cases use, as a translator's output
//! calls them: the externs through `lean_runtime::io`, the Lean-level
//! definitions (`Handle.readToEnd`, `IO.Process.output`, `IO.FS.readFile`)
//! as their Lean code reads, so that their tasks and blocking calls are
//! those of the translated program.

use crate::lean::{as_task, PRIO_DEDICATED};
use lean_runtime::io::process::{self, Child, SpawnArgs, Stdio, StdioConfig};
use lean_runtime::io::{handle, FsMode, Handle, IoError};

pub type R<T> = Result<T, IoError>;

/// `IO.FS.Handle.read h n`: the bytes read (Lean's new `ByteArray`).
pub fn read(h: &Handle, n: usize) -> R<Vec<u8>> {
    handle::check_read_size(n)?;
    let mut v = Vec::new();
    h.read_vec(n, &mut v)?;
    Ok(v)
}

/// `Handle.readBinToEnd` (`readBinToEndInto .empty`): `h.read 1024` until it
/// gives nothing.
pub fn read_bin_to_end(h: &Handle) -> R<Vec<u8>> {
    let mut acc = Vec::new();
    loop {
        let b = read(h, 1024)?;
        if b.is_empty() {
            return Ok(acc);
        }
        acc.extend_from_slice(&b);
    }
}

/// `Handle.readToEnd`: `readBinToEnd`, then `String.fromUTF8?`.
pub fn read_to_end(h: &Handle) -> R<String> {
    let b = read_bin_to_end(h)?;
    String::from_utf8(b)
        .map_err(|_| IoError::user_error("Tried to read from handle containing non UTF-8 data."))
}

/// `Handle.getLine`.
pub fn get_line(h: &Handle) -> R<String> {
    let mut v = Vec::new();
    h.get_line(&mut v)?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// `IO.FS.writeFile`: open with mode `write`, `putStr`, close.
pub fn write_file(path: &str, s: &str) -> R<()> {
    let h = Handle::open(path.as_bytes(), FsMode::Write)?;
    h.put_str(s.as_bytes())
}

/// `IO.FS.readFile` (`readBinFile`, then `String.fromUTF8?`).
pub fn read_file(path: &str) -> R<String> {
    let h = Handle::open(path.as_bytes(), FsMode::Read)?;
    read_to_end(&h)
}

/// `IO.Process.spawn` of `cmd args` with the standard streams `stdio`
/// (Lean's `SpawnArgs` defaults otherwise: no `cwd`, the inherited
/// environment, no `setsid`).
pub fn spawn(cmd: &str, args: &[&str], stdio: StdioConfig) -> R<Child> {
    let args: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
    process::spawn(
        stdio,
        &SpawnArgs {
            cmd: cmd.as_bytes(),
            args: &args,
            cwd: None,
            env: &[],
            inherit_env: true,
            setsid: false,
        },
    )
}

pub const INHERIT: StdioConfig = StdioConfig {
    stdin: Stdio::Inherit,
    stdout: Stdio::Inherit,
    stderr: Stdio::Inherit,
};

/// `IO.Process.Output`.
#[derive(Clone)]
pub struct Output {
    pub exit_code: u32,
    pub stdout: String,
    pub stderr: String,
}

/// `IO.Process.output args` (no input), as its Lean definition reads
/// (`Init/System/IO.lean`): spawn with standard output and error piped and
/// standard input `null`; read standard output on a dedicated task while
/// `main` reads standard error; wait for the child; then the task's result.
pub fn output(cmd: &str, args: &[&str]) -> R<Output> {
    let child = spawn(
        cmd,
        args,
        StdioConfig {
            stdin: Stdio::Null,
            stdout: Stdio::Piped,
            stderr: Stdio::Piped,
        },
    )?;
    let out = child.stdout.clone().expect("piped");
    let stdout = as_task(move || read_to_end(&out), PRIO_DEDICATED);
    let stderr = read_to_end(child.stderr.as_ref().expect("piped"))?;
    let exit_code = child.process.wait()?;
    let stdout = stdout.get()?;
    Ok(Output {
        exit_code,
        stdout,
        stderr,
    })
}

/// Lean's `repr` of a string (`String.quote`).
pub fn quote(s: &str) -> String {
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

/// `s.take n` and `s.drop n` (in characters), as strings.
pub fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn drop_chars(s: &str, n: usize) -> String {
    s.chars().skip(n).collect()
}

/// `IO.Process.getPID`.
pub fn get_pid() -> u32 {
    lean_runtime::io::env::get_pid()
}

/// Lean's `IO.Error.toString`, for the errors the cases print.
pub fn error_text(e: &IoError) -> String {
    let down = |s: &str| {
        let mut c = s.chars();
        match c.next() {
            Some(f) => f.to_lowercase().chain(c).collect::<String>(),
            None => String::new(),
        }
    };
    match e {
        IoError::InvalidArgument(None, c, d) => {
            format!("invalid argument (error code: {c}, {})", down(d))
        }
        e => format!("{e:?}"),
    }
}

/// `repr` of an `Option Unit` and an `Option Int`.
pub fn repr_unit(o: &Option<()>) -> String {
    match o {
        Some(()) => "some ()".into(),
        None => "none".into(),
    }
}

pub fn repr_int(o: &Option<i64>) -> String {
    match o {
        Some(v) if *v < 0 => format!("some ({v})"),
        Some(v) => format!("some {v}"),
        None => "none".into(),
    }
}
