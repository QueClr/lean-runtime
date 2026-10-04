//! The io program cases (`tests/cases/io/*.lean`) on the model: each case
//! (but `borrow_with_ref_struct`, which tests only what a translator
//! generates; see its `.toml`) has a twin here, a Rust function making the same calls through
//! `lean_runtime::io` as the case's Lean program makes through Lean's
//! runtime, with the little a translator's glue adds (`IO.println` is one
//! `putStr` of the line and `\n`; `IO.FS.readFile` and `writeFile` are their
//! Lean definitions; an uncaught error is `show_error` of `IO.Error.toString`
//! and exit status 1; a handle is dropped after its last use, as Lean frees
//! it there).
//!
//! The test runs `scripts/cases.py check` (the checker translators use) on
//! wrappers that start this binary as each case's twin, so every twin's
//! stdout, stderr and exit code must equal the case's expected outcome:
//! native Lean 4.34.0's, or the correct one where native is wrong (LB-02,
//! LB-03 in `docs/lean-bugs.md`; native's is then in the case's `native`
//! field). A twin whose case is missing fails the run (`NO CASE`).
//!
//! The binary runs without libtest (`harness = false`): as a twin it writes
//! only what the program writes. It is each case's twin when started under
//! the case's name (the checker runs symbolic links named after the cases),
//! and then, as a translator's ELF constructor does, opens native Lean's
//! startup descriptors (`io::startup`) before Rust's runtime starts, so that
//! closed standard descriptors are taken as natively.

use lean_runtime::io::{env, exit, fs as lfs, FsMode, Handle, IoError};

// ---- the glue a translator adds ----

type R<T> = Result<T, IoError>;

fn print(s: &str) -> R<()> {
    Handle::stdout().put_str(s.as_bytes())
}

fn println(s: &str) -> R<()> {
    let mut line = String::with_capacity(s.len() + 1);
    line.push_str(s);
    line.push('\n');
    print(&line)
}

fn eprint(s: &str) -> R<()> {
    Handle::stderr().put_str(s.as_bytes())
}

fn eprintln(s: &str) -> R<()> {
    eprint(&format!("{s}\n"))
}

fn open(path: &str, mode: FsMode) -> R<Handle> {
    Handle::open(path.as_bytes(), mode)
}

/// `Handle.read n` as a translator whose `ByteArray` is a `Vec` makes it.
fn read(h: &Handle, n: usize) -> R<Vec<u8>> {
    lean_runtime::io::handle::check_read_size(n)?;
    let mut v = Vec::new();
    h.read_vec(n, &mut v)?;
    Ok(v)
}

fn get_line(h: &Handle) -> R<String> {
    let mut v = Vec::new();
    h.get_line(&mut v)?;
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// `IO.FS.writeFile`: `Handle.mk fname .write`, `putStr`, the handle freed.
fn write_file(path: &str, content: &str) -> R<()> {
    let h = open(path, FsMode::Write)?;
    h.put_str(content.as_bytes())
}

/// `IO.FS.readFile`: `readBinFile` (`metadata`, `Handle.mk .read`, `read` of
/// the size, then `readBinToEndInto`'s `read 1024` until empty).
fn read_file(path: &str) -> R<String> {
    let size = lfs::metadata(path.as_bytes())?.byte_size as usize;
    let h = open(path, FsMode::Read)?;
    let mut data = if size > 0 {
        read(&h, size)?
    } else {
        Vec::new()
    };
    loop {
        let b = read(&h, 1024)?;
        if b.is_empty() {
            break;
        }
        data.extend_from_slice(&b);
    }
    Ok(String::from_utf8(data).expect("UTF-8 file"))
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

/// `String.decapitalize`.
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

/// The end of a native program: `main`'s result, then C's `exit`.
fn finish(r: R<()>) -> ! {
    match r {
        Ok(()) => exit::exit(0),
        Err(e) => {
            exit::show_error(to_string(&e).as_bytes());
            exit::exit(1)
        }
    }
}

/// `try … catch e => return s!"error: {e}"` of the cases' `showErr`.
fn show_err<T>(r: R<T>, fmt: impl FnOnce(T) -> String) -> String {
    match r {
        Ok(v) => fmt(v),
        Err(e) => format!("error: {}", to_string(&e)),
    }
}

fn nat(s: &str) -> usize {
    s.parse().unwrap()
}

// ---- the twins ----

fn exit_flush_order(args: &[String]) -> R<()> {
    let h = open(&args[0], FsMode::Append)?;
    print("A\n")?;
    h.put_str(b"B\n")?;
    let _held = &h;
    exit::exit(0)
}

fn stdout_closed(args: &[String]) -> R<()> {
    println(&format!("hello {}", args.len()))?;
    eprintln("after println")
}

fn write_after_read(args: &[String]) -> R<()> {
    let (size, read_n, write_n) = (nat(&args[0]), nat(&args[1]), nat(&args[2]));
    let f = "re-war.txt";
    write_file(f, &"x".repeat(size))?;
    let h = open(f, FsMode::ReadWrite)?;
    read(&h, read_n)?;
    h.put_str("W".repeat(write_n).as_bytes())?;
    let count_w = |s: String| s.chars().filter(|&c| c == 'W').count();
    println(&format!("before flush {}", count_w(read_file(f)?)))?;
    h.flush()?;
    println(&format!("after flush {}", count_w(read_file(f)?)))?;
    println(&format!("size {}", read_file(f)?.chars().count()))
}

fn stdout_block(args: &[String]) -> R<()> {
    for (i, (a, c)) in args.iter().zip(['a', 'b', 'c', 'd', 'e', 'f']).enumerate() {
        let n = nat(a);
        let mut line = c.to_string().repeat(n - 1);
        line.push('\n');
        print(&line)?;
        eprintln(&format!("STDERR {}", i + 1))?;
    }
    println("end")
}

fn buf_order(args: &[String]) -> R<()> {
    let a: Vec<usize> = args.iter().map(|x| nat(x)).collect();
    let (n, every, big, m, step, modulus) = (a[0], a[1], a[2], a[3], a[4], a[5]);
    for i in 0..n {
        println(&format!("line {i}"))?;
        if i % every == 0 {
            eprintln(&format!("ERR {i}"))?;
        }
    }
    print(&format!("x{}", "y".repeat(big)))?;
    eprintln("ERR big")?;
    println("")?;
    for i in 0..m {
        print(&format!("z{}", "w".repeat(i * step % modulus)))?;
        eprintln(&format!("ERR {i}"))?;
    }
    let out = Handle::stdout();
    out.put_str(b"before flush")?;
    out.flush()?;
    eprintln("ERR after flush")?;
    println("end")
}

fn exit_order_handles(args: &[String]) -> R<()> {
    let path = &args[0];
    let h = open(path, FsMode::Append)?;
    h.put_str(b"B\n")?;
    drop(h);
    eprintln("E1")?;
    let h2 = open(path, FsMode::Append)?;
    h2.put_str(b"C\n")?;
    drop(h2);
    eprintln("E2")?;
    print("A\n")?;
    let a = open(path, FsMode::Append)?;
    let b = open(path, FsMode::Append)?;
    a.put_str(b"alive-a\n")?;
    b.put_str(b"alive-b\n")?;
    print("stdout-2\n")?;
    eprintln("E3")?;
    let _held = (&a, &b);
    exit::exit(3)
}

fn stdin_ahead(args: &[String]) -> R<()> {
    let stdin = Handle::stdin();
    let l = get_line(&stdin)?;
    let b = read(&stdin, nat(&args[0]))?;
    println(&format!("got {} and {} bytes", quote(&l), b.len()))
}

fn exit_flush_handles(args: &[String]) -> R<()> {
    let g = open(&args[0], FsMode::Write)?;
    g.put_str(b"written via the global handle\n")?;
    // kept in the global reference
    let _global = &g;
    let h = open(&args[1], FsMode::Write)?;
    h.put_str(b"line before exit\n")?;
    println("stdout before exit")?;
    let _held = &h;
    exit::exit(7)
}

fn broken_pipe(args: &[String]) -> R<()> {
    for i in 0..nat(&args[0]) {
        println(&format!("line {i}"))?;
    }
    eprintln("finished loop")
}

fn zero_byte_ops(args: &[String]) -> R<()> {
    let stdin = Handle::stdin();
    stdin.put_str(args[0].as_bytes())?;
    stdin.write(args[0].as_bytes())?;
    let l = get_line(&stdin)?;
    println(&format!("got {}", quote(&l)))?;
    let b = read(&Handle::stdout(), args.len() - 1)?;
    println(&format!("read 0 from stdout: {}", b.len()))
}

fn exit_status(args: &[String]) -> R<()> {
    if args[0] == "force" {
        println("buffered, lost")?;
        eprintln("stderr, kept")?;
        exit::force_exit(nat(&args[1]) as u8 as i32);
    }
    print("partial line ")?;
    let (n, stop, code) = (nat(&args[1]), nat(&args[2]), nat(&args[3]) as u8);
    for i in 0..n {
        println(&format!("working {i}"))?;
        if i == stop {
            eprintln("exiting")?;
            exit::exit(code as i32);
        }
    }
    println("never printed")
}

fn write_chunks(args: &[String]) -> R<()> {
    let part = |p: &str| -> String {
        if p == "/" {
            "\n".to_owned()
        } else {
            let c = p.chars().next().unwrap();
            c.to_string().repeat(nat(&p[c.len_utf8()..]))
        }
    };
    let seq: Vec<String> = args
        .iter()
        .map(|a| a.split('+').map(part).collect())
        .collect();
    for s in &seq {
        print(s)?;
        eprint("|")?;
    }
    let h = open("c1.txt", FsMode::Write)?;
    for s in &seq {
        h.put_str(s.as_bytes())?;
    }
    h.flush()?;
    drop(h);
    write_file("c2.txt", &"z".repeat(20000))?;
    let r = open("c2.txt", FsMode::ReadWrite)?;
    get_line(&r)?;
    r.put_str("A".repeat(10).as_bytes())?;
    read(&r, 100)?;
    r.put_str("B".repeat(5000).as_bytes())?;
    read(&r, 3000)?;
    r.put_str("C".repeat(1500).as_bytes())?;
    r.rewind()?;
    r.put_str("D".repeat(4096).as_bytes())?;
    read(&r, 1)?;
    r.put_str(b"E")?;
    r.flush()?;
    drop(r);
    println("")
}

fn rewind_serves_buffer(args: &[String]) -> R<()> {
    let f = "s.txt";
    write_file(f, &args[0])?;
    let h = open(f, FsMode::Read)?;
    println(&format!("first: {}", quote(&get_line(&h)?)))?;
    write_file(f, &args[1])?;
    h.rewind()?;
    println(&format!("after rewind 1: {}", quote(&get_line(&h)?)))?;
    write_file(f, &args[2])?;
    h.rewind()?;
    println(&format!("after rewind 2: {}", quote(&get_line(&h)?)))?;
    println(&format!("rest: {}", quote(&get_line(&h)?)))
}

/// LB-02's case (`io/read_after_write`): the correct outcome, which the case
/// expects.
fn read_after_write(args: &[String]) -> R<()> {
    let t = &args[0];
    let w = open("f.txt", FsMode::Write)?;
    w.put_str(t.as_bytes())?;
    let r = show_err(read(&w, 5000), |b| format!("read {}", b.len()));
    w.flush()?;
    drop(w);
    println(&format!("F: {r}; contents {}", quote(&read_file("f.txt")?)))?;
    write_file("a.txt", "0123")?;
    let a = open("a.txt", FsMode::Append)?;
    a.put_str(t.as_bytes())?;
    let r = show_err(read(&a, 5000), |b| format!("read {}", b.len()));
    a.flush()?;
    drop(a);
    println(&format!("A: {r}; contents {}", quote(&read_file("a.txt")?)))?;
    for (name, n, tag) in [("g.txt", 5000, "G"), ("s.txt", 4, "S")] {
        write_file(name, "0123456789")?;
        let h = open(name, FsMode::ReadWrite)?;
        h.put_str(t.as_bytes())?;
        let b = read(&h, n)?;
        h.flush()?;
        drop(h);
        let b = String::from_utf8(b).unwrap();
        println(&format!(
            "{tag}: read {}; contents {}",
            quote(&b),
            quote(&read_file(name)?)
        ))?;
    }
    let out = Handle::stdout();
    out.flush()?;
    print(&format!("O: {t} printed; "))?;
    let r = show_err(read(&out, 5000), |b| format!("read {}", b.len()));
    println(&r)
}

/// LB-03's case (`io/error_without_file_name`): the correct outcome, which
/// the case expects.
fn error_without_file_name(args: &[String]) -> R<()> {
    let d = format!("gone-{}", args.len());
    lfs::create_dir(d.as_bytes())?;
    let mut abs = Vec::new();
    lfs::real_path(d.as_bytes(), &mut abs)?;
    println("before")?;
    lfs::set_current_dir(&abs)?;
    lfs::remove_dir(&abs)?;
    let mut cwd = Vec::new();
    match lfs::process_current_dir(&mut cwd) {
        Ok(()) => println(&format!("cwd {}", String::from_utf8_lossy(&cwd)))?,
        Err(IoError::NoFileOrDirectory(f, c, m)) => println(&format!(
            "noFileOrDirectory {} {c} {}",
            quote(&f),
            quote(&m)
        ))?,
        Err(e) => println(&format!("other error: {}", to_string(&e)))?,
    }
    println("after")
}

fn startup_fd_limit(args: &[String]) -> R<()> {
    let mut fds: Vec<u64> = Vec::new();
    lfs::read_dir(b"/proc/self/fd", |n| {
        if let Some(v) = std::str::from_utf8(n).ok().and_then(|t| t.parse().ok()) {
            fds.push(v)
        }
    })?;
    fds.sort();
    let list: Vec<String> = fds.iter().map(|n| n.to_string()).collect();
    println(&format!("open at startup: #[{}]", list.join(", ")))?;
    let mut hs = Vec::new();
    let mut err = "limit not reached".to_owned();
    for _ in 0..nat(&args[1]) {
        match open(&args[0], FsMode::Read) {
            Ok(h) => hs.push(h),
            Err(e) => {
                err = to_string(&e);
                break;
            }
        }
    }
    println(&format!("opened {} more, then: {err}", hs.len()))
}

fn startup_closed_stdio(args: &[String]) -> R<()> {
    let report = open(&args[0], FsMode::Write)?;
    let n = nat(&args[1]);
    let mut fds: Vec<u64> = Vec::new();
    lfs::read_dir(b"/proc/self/fd", |name| {
        if let Some(v) = std::str::from_utf8(name).ok().and_then(|t| t.parse().ok()) {
            fds.push(v)
        }
    })?;
    fds.sort();
    let list: Vec<String> = fds.iter().map(|n| n.to_string()).collect();
    let try_io = |r: R<String>| r.unwrap_or_else(|e| format!("error: {}", to_string(&e)));
    let (stdin, stdout, stderr) = (Handle::stdin(), Handle::stdout(), Handle::stderr());
    let r1 = try_io(get_line(&stdin).map(|l| format!("stdin getLine: {}", quote(&l))));
    let r2 = try_io(read(&stdin, 5).map(|b| format!("stdin read: {}", b.len())));
    let r3 = try_io(
        print("out\n")
            .and_then(|()| stdout.flush())
            .map(|()| "stdout: ok".to_owned()),
    );
    let r4 = try_io(
        stderr
            .put_str(b"err\n")
            .and_then(|()| stderr.flush())
            .map(|()| "stderr: ok".to_owned()),
    );
    let r5 = try_io(read(&stdout, 5).map(|b| format!("stdout read: {}", b.len())));
    let r6 = try_io(read(&stdout, n).map(|b| format!("stdout read {n}: {}", b.len())));
    let r7 = try_io(read(&stderr, n).map(|b| format!("stderr read {n}: {}", b.len())));
    report.put_str(
        format!(
            "fds #[{}]\n{r1}\n{r2}\n{r3}\n{r4}\n{r5}\n{r6}\n{r7}\n",
            list.join(", ")
        )
        .as_bytes(),
    )
}

/// `IO.asTask (prio := .dedicated)`: a thread of its own.
fn dedicated_task(body: impl FnOnce() -> R<()> + Send + 'static) -> std::thread::JoinHandle<R<()>> {
    std::thread::spawn(body)
}

fn lock_blocked(args: &[String]) -> R<()> {
    let f = args[0].clone();
    write_file(&f, "")?;
    let a = open(&f, FsMode::ReadWrite)?;
    let b = open(&f, FsMode::ReadWrite)?;
    a.lock(true)?;
    let tb = b.clone();
    let t = dedicated_task(move || {
        tb.lock(true)?;
        println("task: b locked")
    });
    env::sleep(nat(&args[1]) as u32);
    b.put_str(b"x")?;
    b.flush()?;
    println("main: wrote through b while the task waits in b.lock")?;
    a.unlock()?;
    let _ = t.join().unwrap();
    println(&format!("done; file {}", quote(&read_file(&f)?)))
}

fn lock_exit(args: &[String]) -> R<()> {
    let f = args[0].clone();
    write_file(&f, "")?;
    let a = open(&f, FsMode::ReadWrite)?;
    let b = open(&f, FsMode::ReadWrite)?;
    a.lock(true)?;
    let tb = b.clone();
    let _t = dedicated_task(move || {
        tb.lock(true)?;
        println("task: b locked")
    });
    env::sleep(nat(&args[1]) as u32);
    b.put_str(b"y")?;
    println("main: exiting with the task still in b.lock")?;
    let _held = (&a, &b);
    exit::exit(0)
}

fn lock_during_read(args: &[String]) -> R<()> {
    let h = open(&args[0], FsMode::Read)?;
    let th = h.clone();
    let t = dedicated_task(move || {
        let l = get_line(&th)?;
        println(&format!("task: got {}", quote(&l)))
    });
    env::sleep(nat(&args[1]) as u32);
    h.lock(true)?;
    println("main: locked while the task reads")?;
    let _ = t.join().unwrap();
    h.unlock()?;
    println("done")
}

fn realpath_errno(args: &[String]) -> R<()> {
    let h = open(&args[0], FsMode::Read)?;
    if let Err(e) = h.put_str(b"x") {
        println(&format!("putStr: {}", to_string(&e)))?;
    }
    for p in &args[1..] {
        let mut out = Vec::new();
        lfs::real_path(p.as_bytes(), &mut out)?;
        match get_line(&h) {
            Ok(l) => println(&format!("realPath {p}, getLine ok {}", quote(&l)))?,
            Err(e) => println(&format!("realPath {p}, getLine: {}", to_string(&e)))?,
        }
    }
    Ok(())
}

/// `fresh name`: an empty file, then an append handle on it.
fn fresh(name: &str) -> R<Handle> {
    write_file(name, "")?;
    open(name, FsMode::Append)
}

/// The case `io/handle_release_order` (cross-test XT-1): a caller releases
/// the dead handles it lent to a call after the call, last argument first
/// (each variable at its first occurrence; one passed to an owned parameter
/// first is the callee's to release). A handle's buffered text reaches the
/// file when its last reference goes.
fn handle_release_order(args: &[String]) -> R<()> {
    let s = args;
    // `put3 (a b c : IO.FS.Handle) s`: all borrowed
    let put3 = |a: &Handle, b: &Handle, c: &Handle| -> R<()> {
        a.put_str(s[0].as_bytes())?;
        b.put_str(s[1].as_bytes())?;
        c.put_str(s[2].as_bytes())
    };
    let a = fresh("o1.txt")?;
    let b = open("o1.txt", FsMode::Append)?;
    let c = open("o1.txt", FsMode::Append)?;
    put3(&a, &b, &c)?;
    drop(c);
    drop(b);
    drop(a);
    println(&format!("put3: {}", read_file("o1.txt")?))?;
    let w = fresh("o2.txt")?;
    let h1 = open("o2.txt", FsMode::Append)?;
    let h2 = open("o2.txt", FsMode::Append)?;
    // `put4 w h1 h2 h1`
    w.put_str(s[0].as_bytes())?;
    h1.put_str(s[1].as_bytes())?;
    h2.put_str(s[2].as_bytes())?;
    h1.put_str(s[3].as_bytes())?;
    drop(h2);
    drop(h1);
    drop(w);
    println(&format!("put4 w h1 h2 h1: {}", read_file("o2.txt")?))?;
    let x = fresh("o3.txt")?;
    let y = open("o3.txt", FsMode::Append)?;
    // `own3 x y x`: `a` owned (kept in a reference until `own3` returns),
    // `b` and `c` borrowed; the caller then releases `y`, and `x` last
    let own3 = |a: Handle, b: &Handle, c: &Handle| -> R<()> {
        let r = a; // `IO.mkRef a`
        b.put_str(s[1].as_bytes())?;
        c.put_str(s[2].as_bytes())?;
        r.put_str(s[0].as_bytes())
    };
    own3(x.clone(), &y, &x)?;
    drop(y);
    drop(x);
    println(&format!("own3 x y x: {}", read_file("o3.txt")?))?;
    let a = fresh("o4.txt")?;
    let b = open("o4.txt", FsMode::Append)?;
    let c = open("o4.txt", FsMode::Append)?;
    // `apply3 put3 a b c`: `put3._boxed` releases after the call the same way
    put3(&a, &b, &c)?;
    drop(c);
    drop(b);
    drop(a);
    println(&format!("apply3 put3: {}", read_file("o4.txt")?))
}

/// A twin: the case's program over its arguments.
type Twin = fn(&[String]) -> R<()>;

const TWINS: &[(&str, Twin)] = &[
    ("exit_flush_order", exit_flush_order),
    ("stdout_closed", stdout_closed),
    ("write_after_read", write_after_read),
    ("stdout_block", stdout_block),
    ("buf_order", buf_order),
    ("exit_order_handles", exit_order_handles),
    ("stdin_ahead", stdin_ahead),
    ("exit_flush_handles", exit_flush_handles),
    ("broken_pipe", broken_pipe),
    ("zero_byte_ops", zero_byte_ops),
    ("exit_status", exit_status),
    ("write_chunks", write_chunks),
    ("rewind_serves_buffer", rewind_serves_buffer),
    ("read_after_write", read_after_write),
    ("error_without_file_name", error_without_file_name),
    ("startup_fd_limit", startup_fd_limit),
    ("startup_closed_stdio", startup_closed_stdio),
    ("lock_blocked", lock_blocked),
    ("lock_exit", lock_exit),
    ("lock_during_read", lock_during_read),
    ("realpath_errno", realpath_errno),
    ("handle_release_order", handle_release_order),
];

/// The twin named by `argv[0]`'s file name, if any.
fn twin_name(argv0: &[u8]) -> Option<&'static str> {
    let base = argv0.rsplit(|&b| b == b'/').next().unwrap_or(argv0);
    TWINS.iter().map(|(n, _)| *n).find(|n| n.as_bytes() == base)
}

/// The translator's ELF constructor: native Lean's startup descriptors,
/// opened before Rust's runtime replaces closed standard descriptors with
/// `/dev/null`, when this binary runs as a twin (`argv[0]` from
/// `/proc/self/cmdline`: std's own arguments may not be set up yet). Not
/// under Miri, which runs no file system calls in isolation.
#[cfg(not(miri))]
extern "C" fn startup() {
    let Ok(cmdline) = std::fs::read("/proc/self/cmdline") else {
        return;
    };
    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
    if twin_name(argv0).is_some() {
        if let Err(f) = lean_runtime::io::startup::open_native_descriptors() {
            lean_runtime::io::startup::fail_as_native(f);
        }
    }
}

#[cfg(not(miri))]
#[used]
#[link_section = ".init_array"]
static STARTUP: extern "C" fn() = startup;

fn main() {
    use std::os::unix::ffi::OsStrExt;
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if let Some(id) = twin_name(argv0.as_bytes()) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let twin = TWINS.iter().find(|(n, _)| *n == id).unwrap().1;
        finish(twin(&args));
    }
    if cfg!(miri) {
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let exe = std::env::current_exe().expect("test binary path");
    let dir = std::env::temp_dir().join(format!("lean-runtime-twins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (id, _) in TWINS {
        std::os::unix::fs::symlink(&exe, dir.join(id)).unwrap();
    }
    let status = std::process::Command::new("python3")
        .arg(format!("{root}/scripts/cases.py"))
        .args(["check", "--exe-dir"])
        .arg(&dir)
        .args(TWINS.iter().map(|(id, _)| *id))
        .env("LEAN_RUNTIME_NO_CAP", "1")
        .status()
        .expect("python3 scripts/cases.py");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        status.success(),
        "a model twin differs from native Lean (see above)"
    );
}
