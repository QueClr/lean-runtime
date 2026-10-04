//! `Std.Internal.UV.System`'s queries: Lean 4.34.0's `src/runtime/uv/system.cpp`
//! over libuv 1.48 (`src/unix/core.c`, `linux.c`, `proctitle.c`,
//! `procfs-exepath.c`, `random-getrandom.c`), through `std`, `nix`'s and
//! `rustix`'s safe wrappers, `/proc` and `/sys`.
//!
//! A libuv error `-e` is Lean's `lean_decode_uv_error(-e, nullptr)`
//! ([`IoError::decode_uv_error`]); where Lean builds that error over a null
//! file name and crashes (`UV_ENOENT`: a removed working directory, a user
//! without a password entry), the error has the empty name (LB-03). Results
//! of unbounded size are appended to the caller's [`ByteSink`]; the
//! structures (`CPUInfo`, `PasswdInfo`, `GroupInfo`, `UnameInfo`, `RUsage`)
//! are small owned values. Strings are bytes; the caller decodes them as
//! Lean's `lean_mk_string` does (lossily).
//!
//! `random`'s promise is the scheduler's: [`random_check`] gives the error
//! Lean returns at once, [`random_fill`] the bytes the promise resolves with.
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/uvsys.rs` (the libuv ports of
//! `uv_cpu_info`, the memory and cgroup queries, `uv__slurp`, `scanf`; the
//! `getent` route for user names; probes `validate/io/uv_system`, A756) and
//! lean2rr's `runtime/leanrt/src/sys.rs` (`uv_set_process_title` over the
//! arguments' memory, `uv_os_get_group`, `uv_random`; tests `RtSystem`,
//! `RtUvSysLimits`, `RtOsStringsLossy`, `RtCwdLong`).

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::sync::{Mutex, PoisonError};

use super::error::{set_errno, IoError, E2BIG, EINTR, EINVAL, ENOBUFS, ENOENT, ERANGE, ESRCH};
use super::{environ, ByteSink};

/// `PATH_MAX`, the buffer `system.cpp` gives `uv_cwd`, `uv_os_homedir`,
/// `uv_os_tmpdir` and `uv_exepath`.
const PATH_MAX: usize = 4096;

/// `lean_decode_uv_error(-e, nullptr)` of a system call's failure, with the
/// modelled `errno` set to `e`, as the call sets C's.
fn uv(e: i32) -> IoError {
    set_errno(e);
    IoError::decode_uv_error(-e, None)
}

/// `lean_decode_uv_error(-e, nullptr)` of an error libuv makes itself (a
/// buffer too small, a range check): `errno` keeps its value.
fn libuv(e: i32) -> IoError {
    IoError::decode_uv_error(-e, None)
}

/// The `errno` of a failed `std` call, set in the model.
fn failed(e: &std::io::Error) -> i32 {
    let code = e.raw_os_error().unwrap_or(EINVAL);
    set_errno(code);
    code
}

fn std_err(e: &std::io::Error) -> IoError {
    uv(failed(e))
}

/// `uv__slurp`: one `read` of at most `len - 1` bytes; `None` when the file
/// cannot be opened or read (the failing call's `errno` set).
fn slurp(path: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut f = File::open(OsStr::from_bytes(path))
        .map_err(|e| failed(&e))
        .ok()?;
    let mut buf = vec![0u8; len - 1];
    let n = loop {
        match f.read(&mut buf) {
            Ok(n) => break n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                failed(&e);
                return None;
            }
        }
    };
    buf.truncate(n);
    Some(buf)
}

// ---- the process title (`uv_setup_args`, `uv_get_process_title`, `uv_set_process_title`) ----

/// libuv's `process_title`: the title, and the memory of the original
/// arguments it may grow over (`pt.cap`: every argument's bytes and NUL).
struct Title {
    title: Vec<u8>,
    cap: usize,
}

static TITLE: Mutex<Option<Option<Title>>> = Mutex::new(None);

/// The title as `uv_setup_args` sets it up from `argv` (Lean's
/// `lean_setup_args` calls it before `main`): `argv[0]`; none (`args_mem`
/// null) without arguments.
fn with_title<R>(f: impl FnOnce(Option<&mut Title>) -> R) -> R {
    let mut g = TITLE.lock().unwrap_or_else(PoisonError::into_inner);
    let t = g.get_or_insert_with(|| {
        let args: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
        let first = args.first()?.clone();
        Some(Title {
            title: first,
            cap: args.iter().map(|a| a.len() + 1).sum(),
        })
    });
    f(t.as_mut())
}

/// `getProcessTitle` (`uv_get_process_title` into 512 bytes): the title; one
/// of 512 bytes or more, or no arguments, is `UV_ENOBUFS`.
pub fn get_process_title<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    with_title(|t| match t {
        Some(t) if t.title.len() < 512 => {
            out.extend_from_slice(&t.title);
            Ok(())
        }
        _ => Err(libuv(ENOBUFS)),
    })
}

/// `setProcessTitle` (`uv_set_process_title`): a title holding a NUL byte is
/// Lean's embedded-NUL error; otherwise it is cut to the arguments' memory
/// less one byte and becomes the calling thread's name (`prctl(PR_SET_NAME)`,
/// its first 15 bytes). Native libuv also writes it over the original
/// `argv` strings (`/proc/self/cmdline`), which safe code cannot do.
pub fn set_process_title(title: &[u8]) -> Result<(), IoError> {
    if title.contains(&0) {
        return Err(IoError::embedded_nul(title));
    }
    with_title(|t| {
        let Some(t) = t else {
            return Err(libuv(ENOBUFS));
        };
        let len = if title.len() >= t.cap {
            t.cap.saturating_sub(1)
        } else {
            title.len()
        };
        t.title = title[..len].to_vec();
        if let Ok(name) = std::ffi::CString::new(t.title.clone()) {
            let _ = nix::sys::prctl::set_name(&name);
        }
        Ok(())
    })
}

// ---- simple values ----

/// `osGetPid` (`uv_os_getpid`).
pub fn os_getpid() -> u64 {
    u64::from(std::process::id())
}

/// `osGetPpid` (`uv_os_getppid`).
pub fn os_getppid() -> u64 {
    u64::from(std::os::unix::process::parent_id())
}

/// `uptime` (`uv_uptime`): the first number of `/proc/uptime` (`%lf` of at
/// most 127 bytes), cut to an integer as C's conversion of a `double` to
/// `uint64_t` cuts it; without that file, `CLOCK_BOOTTIME`'s seconds.
pub fn uptime() -> Result<u64, IoError> {
    if let Some(buf) = slurp(b"/proc/uptime", 128) {
        if let Some(x) = scan_f64(&buf) {
            return Ok(x as u64);
        }
    }
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    Ok(t.tv_sec as f64 as u64)
}

/// `sscanf("%lf")` of the leading decimal number after white space.
fn scan_f64(buf: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(buf).ok()?;
    let s = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let end = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0))
        .map(|(i, c)| i + c.len_utf8())
        .last()?;
    s[..end].parse::<f64>().ok()
}

/// `hrtime` (`uv_hrtime`): `CLOCK_MONOTONIC` in nanoseconds.
pub fn hrtime() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    (t.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(t.tv_nsec as u64)
}

// ---- directories ----

/// `cwd` (`uv_cwd` into `PATH_MAX` bytes): `getcwd`, one trailing `/` dropped
/// from a longer path; a path of exactly `PATH_MAX` bytes is `UV_ENOBUFS` (it
/// fits libuv's scratch buffer), a longer one `ERANGE`; a failing `getcwd`
/// is its error (`ENOENT` in a removed directory, with the empty name: LB-03).
/// The modelled `errno` for a path of `PATH_MAX` bytes or more: the kernel's
/// `getcwd` refuses it (`ENAMETOOLONG`) and glibc's generic `getcwd` walks
/// the tree, clearing `errno` before each `readdir`, so libuv's second call
/// leaves 0 when it fits its scratch buffer (`ENOBUFS`) and `ERANGE` when not
/// (native case `uvsys/errno_after`). Never during a spawn that has the
/// process in its `cwd` (`process::with_cwd_read`).
pub fn cwd<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    let mut p = super::process::with_cwd_read(std::env::current_dir)
        .map_err(|e| std_err(&e))?
        .into_os_string()
        .into_vec();
    if p.len() >= PATH_MAX {
        let (e, code) = if p.len() == PATH_MAX {
            (0, ENOBUFS)
        } else {
            (ERANGE, ERANGE)
        };
        set_errno(e);
        return Err(libuv(code));
    }
    if p.len() > 1 && p.last() == Some(&b'/') {
        p.pop();
    }
    out.extend_from_slice(&p);
    Ok(())
}

/// `chdir` (`uv_chdir`): a path holding a NUL byte is Lean's embedded-NUL
/// error; an error names the path. Never during a spawn that has the process
/// in its `cwd` (`process::with_cwd_change`).
pub fn chdir(path: &[u8]) -> Result<(), IoError> {
    if path.contains(&0) {
        return Err(IoError::embedded_nul(path));
    }
    super::process::with_cwd_change(|| rustix::process::chdir(path)).map_err(|e| {
        set_errno(e.raw_os_error());
        IoError::decode_uv_error(-e.raw_os_error(), Some(path))
    })
}

/// `exePath` (`uv_exepath`, `procfs-exepath.c`): `readlink("/proc/self/exe")`
/// into `PATH_MAX - 1` bytes, a longer target cut there as `readlink` cuts it.
pub fn exepath<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    let p = std::fs::read_link("/proc/self/exe").map_err(|e| std_err(&e))?;
    let b = p.as_os_str().as_bytes();
    out.extend_from_slice(&b[..b.len().min(PATH_MAX - 1)]);
    Ok(())
}

/// libuv's `uv_os_tmpdir` into `PATH_MAX` bytes: the first of `TMPDIR`, `TMP`,
/// `TEMP`, `TEMPDIR` that is set (even to the empty string), else `/tmp`; a
/// value of `PATH_MAX` bytes or more is `UV_ENOBUFS` (the error code `-e`);
/// one trailing `/` dropped from a longer value.
pub(crate) fn uv_os_tmpdir() -> Result<Vec<u8>, i32> {
    let mut dir = ["TMPDIR", "TMP", "TEMP", "TEMPDIR"]
        .iter()
        .find_map(|v| environ::getenv(v.as_bytes()))
        .unwrap_or_else(|| b"/tmp".to_vec());
    if dir.len() >= PATH_MAX {
        return Err(ENOBUFS);
    }
    if dir.len() > 1 && dir.last() == Some(&b'/') {
        dir.pop();
    }
    Ok(dir)
}

/// `osTmpdir` (`uv_os_tmpdir`).
pub fn os_tmpdir<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    let d = uv_os_tmpdir().map_err(libuv)?;
    out.extend_from_slice(&d);
    Ok(())
}

/// `osHomedir` (`uv_os_homedir` into `PATH_MAX` bytes): `HOME` when it is set
/// (even to the empty string), else the effective user's home directory;
/// `PATH_MAX` bytes or more is `UV_ENOBUFS`.
pub fn os_homedir<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    let dir = match environ::getenv(b"HOME") {
        Some(h) => h,
        None => passwd_entry()?.dir.into_os_string().into_vec(),
    };
    if dir.len() >= PATH_MAX {
        return Err(libuv(ENOBUFS));
    }
    out.extend_from_slice(&dir);
    Ok(())
}

// ---- users and groups ----

/// Lean's `Std.Internal.UV.System.PasswdInfo`, its fields in Lean's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswdInfo {
    pub username: Vec<u8>,
    pub uid: Option<u64>,
    pub gid: Option<u64>,
    pub shell: Option<Vec<u8>>,
    pub homedir: Option<Vec<u8>>,
}

/// Lean's `Std.Internal.UV.System.GroupInfo`, its fields in Lean's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub groupname: Vec<u8>,
    pub gid: u64,
    pub members: Vec<Vec<u8>>,
}

/// `uv__getpwuid_r(geteuid())`: the effective user's entry; none is
/// `UV_ENOENT` (with the empty name, LB-03). glibc's `getpwuid_r` leaves
/// `errno` at its result: 0 for an entry or none, else the error.
fn passwd_entry() -> Result<nix::unistd::User, IoError> {
    match nix::unistd::User::from_uid(nix::unistd::Uid::effective()) {
        Ok(Some(u)) => {
            set_errno(0);
            Ok(u)
        }
        Ok(None) => {
            set_errno(0);
            Err(libuv(ENOENT))
        }
        Err(e) => Err(uv(e as i32)),
    }
}

/// The fields of `getent <db> <id>`'s line, as bytes: `nix` gives names
/// decoded by Rust's lossy rule, which keeps valid UTF-8 as it is, so only a
/// name holding U+FFFD is read again, as bytes, from `getent` (the same NSS
/// lookup), and decoded by the caller with Lean's rule. Without `getent`,
/// `nix`'s decoding stays (leanrs DV15 (c)).
fn getent(db: &str, id: u32) -> Option<Vec<Vec<u8>>> {
    for exe in ["/usr/bin/getent", "/bin/getent"] {
        let out = std::process::Command::new(exe)
            .args([db, &id.to_string()])
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                let line = o.stdout.split(|&c| c == b'\n').next()?;
                return Some(line.split(|&c| c == b':').map(<[u8]>::to_vec).collect());
            }
        }
    }
    None
}

fn lossless(name: String, exact: impl FnOnce() -> Option<Vec<u8>>) -> Vec<u8> {
    if name.contains('\u{FFFD}') {
        if let Some(b) = exact() {
            return b;
        }
    }
    name.into_bytes()
}

/// `osGetPasswd` (`uv_os_get_passwd`): the effective user's name, ids, shell
/// and home directory (uid and gid are always `some`).
pub fn os_get_passwd() -> Result<PasswdInfo, IoError> {
    let u = passwd_entry()?;
    let uid = u.uid.as_raw();
    Ok(PasswdInfo {
        username: lossless(u.name, || getent("passwd", uid)?.into_iter().next()),
        uid: Some(u64::from(uid)),
        gid: Some(u64::from(u.gid.as_raw())),
        shell: Some(u.shell.into_os_string().into_vec()),
        homedir: Some(u.dir.into_os_string().into_vec()),
    })
}

/// `osGetGroup gid` (`uv_os_get_group`, the gid cut to a `uid_t`): the group's
/// name, id and members; `none` for no such group (`UV_ENOENT`); another
/// error is decoded with the file name `group`, as `system.cpp` does.
pub fn os_get_group(gid: u64) -> Result<Option<GroupInfo>, IoError> {
    let gid = gid as u32;
    // glibc's `getgrgid_r` leaves `errno` at its result
    let found = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid));
    set_errno(found.as_ref().map_or_else(|e| *e as i32, |_| 0));
    match found {
        Ok(Some(g)) => {
            let exact = std::cell::OnceCell::new();
            let fields = || exact.get_or_init(|| getent("group", gid)).clone();
            let groupname = lossless(g.name, || fields()?.into_iter().next());
            let members = if g.mem.iter().any(|m| m.contains('\u{FFFD}')) {
                match fields().and_then(|f| f.get(3).cloned()) {
                    Some(list) if !list.is_empty() => {
                        list.split(|&c| c == b',').map(<[u8]>::to_vec).collect()
                    }
                    _ => g.mem.into_iter().map(String::into_bytes).collect(),
                }
            } else {
                g.mem.into_iter().map(String::into_bytes).collect()
            };
            Ok(Some(GroupInfo {
                groupname,
                gid: u64::from(g.gid.as_raw()),
                members,
            }))
        }
        // `getgrgid_r` finds no group (some NSS modules say so with `ENOENT`):
        // `UV_ENOENT`, which `system.cpp` turns into `none`
        Ok(None) | Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(e) => Err(IoError::decode_uv_error(-(e as i32), Some(b"group"))),
    }
}

// ---- the environment ----

/// `osEnviron` (`uv_os_environ`): every `environ` entry holding a `=`, in
/// order, split at its first `=` (an entry `=x` is the empty name with value
/// `x`), passed to `each` as (name, value).
pub fn os_environ(mut each: impl FnMut(&[u8], &[u8])) {
    for e in environ::entries() {
        if let Some(at) = e.iter().position(|&c| c == b'=') {
            each(&e[..at], &e[at + 1..]);
        }
    }
}

/// `osGetenv` (`uv_os_getenv`, `getenv`): the value appended to `out` and
/// `true`; `false` for a name holding a NUL byte or an unset one.
pub fn os_getenv<S: ByteSink + ?Sized>(name: &[u8], out: &mut S) -> bool {
    if name.contains(&0) {
        return false;
    }
    match environ::getenv(name) {
        Some(v) => {
            out.extend_from_slice(&v);
            true
        }
        None => false,
    }
}

/// `osSetenv` (`uv_os_setenv`, `setenv(name, value, 1)`): a NUL byte in the
/// name, then in the value, is Lean's embedded-NUL error naming that string;
/// a name `setenv` refuses (empty or holding `=`) is `EINVAL`.
pub fn os_setenv(name: &[u8], value: &[u8]) -> Result<(), IoError> {
    if name.contains(&0) {
        return Err(IoError::embedded_nul(name));
    }
    if value.contains(&0) {
        return Err(IoError::embedded_nul(value));
    }
    if !environ::name_ok(name) {
        return Err(uv(EINVAL));
    }
    environ::set(name, value);
    Ok(())
}

/// `osUnsetenv` (`uv_os_unsetenv`, `unsetenv`): a NUL byte in the name is
/// Lean's embedded-NUL error; a name `unsetenv` refuses is `EINVAL`.
pub fn os_unsetenv(name: &[u8]) -> Result<(), IoError> {
    if name.contains(&0) {
        return Err(IoError::embedded_nul(name));
    }
    if !environ::name_ok(name) {
        return Err(uv(EINVAL));
    }
    environ::unset(name);
    Ok(())
}

// ---- the system ----

/// `osGetHostname` (`uv_os_gethostname`): `gethostname`; Linux's names (at
/// most 64 bytes) fit Lean's 256-byte buffer.
pub fn os_gethostname<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    let h = nix::unistd::gethostname().map_err(|e| uv(e as i32))?;
    out.extend_from_slice(h.as_bytes());
    Ok(())
}

/// Lean's `Std.Internal.UV.System.UnameInfo`, its fields in Lean's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnameInfo {
    pub sysname: Vec<u8>,
    pub release: Vec<u8>,
    pub version: Vec<u8>,
    pub machine: Vec<u8>,
}

/// `osUname` (`uv_os_uname`): `uname`'s system name, release, version and
/// machine.
pub fn os_uname() -> Result<UnameInfo, IoError> {
    let u = nix::sys::utsname::uname().map_err(|e| uv(e as i32))?;
    Ok(UnameInfo {
        sysname: u.sysname().as_bytes().to_vec(),
        release: u.release().as_bytes().to_vec(),
        version: u.version().as_bytes().to_vec(),
        machine: u.machine().as_bytes().to_vec(),
    })
}

/// The process id of a priority call: Lean's `uint64_t` cut to a C `int`
/// (`uv_pid_t`). `None` is 0, the calling process. A negative id names no
/// process: the kernel finds no task for it (`ESRCH`), returned here
/// without the call (`rustix`'s `Pid` holds positive ids only).
fn priority_pid(pid: u64) -> Result<Option<rustix::process::Pid>, IoError> {
    let pid = pid as u32 as i32;
    if pid < 0 {
        return Err(uv(ESRCH));
    }
    Ok(rustix::process::Pid::from_raw(pid))
}

/// `osGetPriority` (`uv_os_getpriority`): `getpriority(PRIO_PROCESS, (int)
/// pid)`, the nice value (Lean boxes the C `int` sign-extended as an
/// `Int64`). libuv clears `errno` before the call, so a success leaves 0.
pub fn os_getpriority(pid: u64) -> Result<i64, IoError> {
    let pid = priority_pid(pid)?;
    let p = rustix::process::getpriority_process(pid).map_err(|e| uv(e.raw_os_error()))?;
    set_errno(0);
    Ok(i64::from(p))
}

/// `osSetPriority` (`uv_os_setpriority`): the priority cut to a C `int`; one
/// outside libuv's `UV_PRIORITY_HIGHEST` (-20) to `UV_PRIORITY_LOW` (19) is
/// `UV_EINVAL`; then `setpriority(PRIO_PROCESS, (int) pid, priority)`.
pub fn os_setpriority(pid: u64, priority: i64) -> Result<(), IoError> {
    let priority = priority as i32;
    if !(-20..=19).contains(&priority) {
        return Err(libuv(EINVAL));
    }
    let pid = priority_pid(pid)?;
    rustix::process::setpriority_process(pid, priority).map_err(|e| uv(e.raw_os_error()))
}

/// Lean's `Std.Internal.UV.System.RUsage`: the times in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RUsage {
    pub user_time: u64,
    pub system_time: u64,
    pub max_rss: u64,
    pub ix_rss: u64,
    pub id_rss: u64,
    pub is_rss: u64,
    pub min_flt: u64,
    pub maj_flt: u64,
    pub n_swap: u64,
    pub in_block: u64,
    pub out_block: u64,
    pub msg_sent: u64,
    pub msg_recv: u64,
    pub signals: u64,
    pub voluntary_cs: u64,
    pub involuntary_cs: u64,
}

/// `getrusage` (`uv_getrusage`, `RUSAGE_SELF`): the two times as
/// `tv_sec * 1000 + tv_usec / 1000`, the other fields as the kernel reports
/// them (`ru_maxrss` in KiB).
pub fn getrusage() -> Result<RUsage, IoError> {
    use nix::sys::resource::{getrusage as rusage, UsageWho};
    let u = rusage(UsageWho::RUSAGE_SELF).map_err(|e| uv(e as i32))?;
    let ms = |t: nix::sys::time::TimeVal| {
        (t.tv_sec() as u64)
            .wrapping_mul(1000)
            .wrapping_add(t.tv_usec() as u64 / 1000)
    };
    Ok(RUsage {
        user_time: ms(u.user_time()),
        system_time: ms(u.system_time()),
        max_rss: u.max_rss() as u64,
        ix_rss: u.shared_integral() as u64,
        id_rss: u.unshared_data_integral() as u64,
        is_rss: u.unshared_stack_integral() as u64,
        min_flt: u.minor_page_faults() as u64,
        maj_flt: u.major_page_faults() as u64,
        n_swap: u.full_swaps() as u64,
        in_block: u.block_reads() as u64,
        out_block: u.block_writes() as u64,
        msg_sent: u.ipc_sends() as u64,
        msg_recv: u.ipc_receives() as u64,
        signals: u.signals() as u64,
        voluntary_cs: u.voluntary_context_switches() as u64,
        involuntary_cs: u.involuntary_context_switches() as u64,
    })
}

/// `random size`'s check before its promise (`lean_uv_random`, `uv_random`):
/// more than `0x7FFFFFFF` bytes is `UV_E2BIG`. Lean allocates the byte array
/// first; the translator does, then calls [`random_fill`] on it.
pub fn random_check(size: u64) -> Result<(), IoError> {
    if size > 0x7FFF_FFFF {
        return Err(libuv(E2BIG));
    }
    Ok(())
}

/// The bytes `random`'s promise resolves with (libuv's `uv__random`:
/// `getrandom`, here the same kernel source read through `/dev/urandom`);
/// an error is the promise's.
pub fn random_fill(buf: &mut [u8]) -> Result<(), IoError> {
    // libuv fills it on its thread pool: the calling thread's `errno` stays
    let os = |e: std::io::Error| libuv(e.raw_os_error().unwrap_or(EINVAL));
    if buf.is_empty() {
        return Ok(());
    }
    let mut f = File::open("/dev/urandom").map_err(os)?;
    let mut done = 0;
    while done < buf.len() {
        match f.read(&mut buf[done..]) {
            Ok(0) => return Err(libuv(EINVAL)),
            Ok(n) => done += n,
            Err(e) if e.raw_os_error() == Some(EINTR) => {}
            Err(e) => return Err(os(e)),
        }
    }
    Ok(())
}

// ---- memory (`uv_get_free_memory` & co., libuv 1.48's `linux.c`) ----

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// `scanf`'s `%lu`/`%llu` at `*i`: white space, an optional sign, at least
/// one digit; the value as `strtoull` gives it (saturated, negated modulo
/// 2^64 after a `-`).
fn scan_unsigned(b: &[u8], i: &mut usize) -> Option<u64> {
    while *i < b.len() && is_c_space(b[*i]) {
        *i += 1;
    }
    let neg = match b.get(*i) {
        Some(b'-') => {
            *i += 1;
            true
        }
        Some(b'+') => {
            *i += 1;
            false
        }
        _ => false,
    };
    let start = *i;
    let mut v: u64 = 0;
    let mut over = false;
    while *i < b.len() && b[*i].is_ascii_digit() {
        let d = u64::from(b[*i] - b'0');
        match v.checked_mul(10).and_then(|x| x.checked_add(d)) {
            Some(x) => v = x,
            None => over = true,
        }
        *i += 1;
    }
    if *i == start {
        return None;
    }
    let v = if over { u64::MAX } else { v };
    Some(if neg && !over { v.wrapping_neg() } else { v })
}

/// `uv__read_proc_meminfo(what)`: the number after the first `what` in at
/// most 4095 bytes of `/proc/meminfo`, in KiB, times 1024; 0 without it.
fn read_proc_meminfo(what: &str) -> u64 {
    let Some(buf) = slurp(b"/proc/meminfo", 4096) else {
        return 0;
    };
    let Some(at) = find(&buf, what.as_bytes()) else {
        return 0;
    };
    let mut i = at + what.len();
    scan_unsigned(&buf, &mut i).unwrap_or(0).wrapping_mul(1024)
}

/// `freeMemory` (`uv_get_free_memory`): `/proc/meminfo`'s `MemAvailable`,
/// else `sysinfo`'s free RAM, else 0.
pub fn free_memory() -> u64 {
    let rc = read_proc_meminfo("MemAvailable:");
    if rc != 0 {
        return rc;
    }
    sysinfo().map_or(0, |i| i.ram_unused())
}

/// `sysinfo`, a failure's `errno` set.
fn sysinfo() -> Option<nix::sys::sysinfo::SysInfo> {
    nix::sys::sysinfo::sysinfo()
        .map_err(|e| set_errno(e as i32))
        .ok()
}

/// `totalMemory` (`uv_get_total_memory`): `/proc/meminfo`'s `MemTotal`, else
/// `sysinfo`'s total RAM, else 0.
pub fn total_memory() -> u64 {
    let rc = read_proc_meminfo("MemTotal:");
    if rc != 0 {
        return rc;
    }
    sysinfo().map_or(0, |i| i.ram_total())
}

/// `uv__read_uint64`: the number at the start of at most 31 bytes of `path`;
/// `max\n` is `u64::MAX`; 0 when the file cannot be read or holds neither.
fn read_uint64(path: &[u8]) -> u64 {
    let Some(buf) = slurp(path, 32) else { return 0 };
    let mut i = 0;
    match scan_unsigned(&buf, &mut i) {
        Some(v) => v,
        None if buf == b"max\n" => u64::MAX,
        None => 0,
    }
}

/// `snprintf(filename, 4097, "<prefix>%.*s<suffix>", n, p)`: at most 4096
/// bytes.
fn cgroup_path(prefix: &str, p: &[u8], suffix: &str) -> Vec<u8> {
    let mut f = prefix.as_bytes().to_vec();
    f.extend_from_slice(p);
    f.extend_from_slice(suffix.as_bytes());
    f.truncate(PATH_MAX);
    f
}

/// `strcspn(p, "\n")` over `p`, which ends where the C string ends.
fn line_len(p: &[u8]) -> usize {
    p.iter()
        .position(|&b| b == b'\n' || b == 0)
        .unwrap_or(p.len())
}

/// `uv__cgroup1_find_memory_controller`: the path after the first
/// `:memory:/` that starts at a colon `strchr` reaches line by line.
fn cgroup1_memory_controller(buf: &[u8]) -> Option<&[u8]> {
    let colon = |from: usize| {
        buf[from..]
            .iter()
            .position(|&b| b == b':')
            .map(|k| from + k)
    };
    let mut p = colon(0);
    while let Some(at) = p {
        if buf[at..].starts_with(b":memory:") {
            let start = (at + ":memory:/".len()).min(buf.len());
            let rest = &buf[start..];
            return Some(&rest[..line_len(rest)]);
        }
        p = buf[at..]
            .iter()
            .position(|&b| b == b'\n')
            .and_then(|k| colon(at + k));
    }
    None
}

/// `uv__get_cgroup1_memory_limits`: the soft and hard limits of the memory
/// controller's cgroup, else of the global controller; cgroup1's maximum
/// (`LONG_MAX` rounded down to a page) is `u64::MAX`.
fn cgroup1_limits(buf: &[u8]) -> (u64, u64) {
    let mut high = 0;
    let mut max = 0;
    let mut found = false;
    if let Some(p) = cgroup1_memory_controller(buf) {
        high = read_uint64(&cgroup_path(
            "/sys/fs/cgroup/memory/",
            p,
            "/memory.soft_limit_in_bytes",
        ));
        max = read_uint64(&cgroup_path(
            "/sys/fs/cgroup/memory/",
            p,
            "/memory.limit_in_bytes",
        ));
        found = high != 0 && max != 0;
    }
    if !found {
        high = read_uint64(b"/sys/fs/cgroup/memory/memory.soft_limit_in_bytes");
        max = read_uint64(b"/sys/fs/cgroup/memory/memory.limit_in_bytes");
    }
    let page = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
        .ok()
        .flatten()
        .unwrap_or(-1);
    let cgroup1_max = (i64::MAX & !(page.wrapping_sub(1))) as u64;
    let fix = |v: u64| if v == cgroup1_max { u64::MAX } else { v };
    (fix(high), fix(max))
}

/// `uv__get_cgroup2_memory_limits`: `memory.max` and `memory.high` of the one
/// cgroup2 entry's path.
fn cgroup2_limits(buf: &[u8]) -> (u64, u64) {
    let p = &buf[4..];
    let p = &p[..line_len(p)];
    let max = read_uint64(&cgroup_path("/sys/fs/cgroup/", p, "/memory.max"));
    let high = read_uint64(&cgroup_path("/sys/fs/cgroup/", p, "/memory.high"));
    (high, max)
}

/// `uv__get_cgroup_constrained_memory`: the smaller limit, 0 when either is 0.
fn cgroup_constrained(buf: &[u8]) -> u64 {
    let (high, max) = if buf.starts_with(b"0::/") {
        cgroup2_limits(buf)
    } else {
        cgroup1_limits(buf)
    };
    if high == 0 || max == 0 {
        0
    } else {
        high.min(max)
    }
}

/// `constrainedMemory` (`uv_get_constrained_memory`): the memory limit of the
/// process's cgroup from at most 1023 bytes of `/proc/self/cgroup`; 0 when
/// there is none or the file cannot be read.
pub fn constrained_memory() -> u64 {
    slurp(b"/proc/self/cgroup", 1024).map_or(0, |buf| cgroup_constrained(&buf))
}

/// `availableMemory` (`uv_get_available_memory`): the cgroup's limit less its
/// current usage (0 when usage exceeds it), else `freeMemory` when there is
/// no limit or the limit exceeds the total memory.
pub fn available_memory() -> u64 {
    let Some(buf) = slurp(b"/proc/self/cgroup", 1024) else {
        return 0;
    };
    let constrained = cgroup_constrained(&buf);
    if constrained == 0 || constrained > total_memory() {
        return free_memory();
    }
    let current = if buf.starts_with(b"0::/") {
        let p = &buf[4..];
        read_uint64(&cgroup_path(
            "/sys/fs/cgroup/",
            &p[..line_len(p)],
            "/memory.current",
        ))
    } else {
        let mut c = 0;
        if let Some(p) = cgroup1_memory_controller(&buf) {
            c = read_uint64(&cgroup_path(
                "/sys/fs/cgroup/memory/",
                p,
                "/memory.usage_in_bytes",
            ));
        }
        if c == 0 {
            c = read_uint64(b"/sys/fs/cgroup/memory/memory.usage_in_bytes");
        }
        c
    };
    constrained.saturating_sub(current)
}

// ---- CPUs (`uv_cpu_info`, libuv 1.48's `linux.c`) ----

/// Lean's `Std.Internal.UV.System.CPUTimes`: milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub sys: u64,
    pub idle: u64,
    pub irq: u64,
}

/// Lean's `Std.Internal.UV.System.CPUInfo`, its fields in Lean's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuInfo {
    pub model: Vec<u8>,
    pub speed: u64,
    pub times: CpuTimes,
}

/// libuv's line of `/proc/cpuinfo` naming a CPU's model on AArch64 (each
/// target uses one marker and table; the tests use both).
#[allow(dead_code)]
const AARCH64_MARKER: &[u8] = b"CPU part\t: ";
/// libuv's line naming a CPU's model on x86-64 (and the other targets
/// without a line of their own).
#[allow(dead_code)]
const X86_MARKER: &[u8] = b"model name\t: ";
/// The marker of the build's target.
#[cfg(target_arch = "aarch64")]
const MODEL_MARKER: &[u8] = AARCH64_MARKER;
#[cfg(not(target_arch = "aarch64"))]
const MODEL_MARKER: &[u8] = X86_MARKER;

/// libuv 1.48's table of AArch64 CPU part codes and model names.
#[allow(dead_code)]
const AARCH64_PARTS: &[u8] = b"0x811\nARM810\n0x920\nARM920\n0x922\nARM922\n0x926\nARM926\n0x940\nARM940\n0x946\nARM946\n\
0x966\nARM966\n0xa20\nARM1020\n0xa22\nARM1022\n0xa26\nARM1026\n0xb02\nARM11 MPCore\n0xb36\nARM1136\n\
0xb56\nARM1156\n0xb76\nARM1176\n0xc05\nCortex-A5\n0xc07\nCortex-A7\n0xc08\nCortex-A8\n0xc09\nCortex-A9\n\
0xc0d\nCortex-A17\n0xc0f\nCortex-A15\n0xc0e\nCortex-A17\n0xc14\nCortex-R4\n0xc15\nCortex-R5\n\
0xc17\nCortex-R7\n0xc18\nCortex-R8\n0xc20\nCortex-M0\n0xc21\nCortex-M1\n0xc23\nCortex-M3\n\
0xc24\nCortex-M4\n0xc27\nCortex-M7\n0xc60\nCortex-M0+\n0xd01\nCortex-A32\n0xd03\nCortex-A53\n\
0xd04\nCortex-A35\n0xd05\nCortex-A55\n0xd06\nCortex-A65\n0xd07\nCortex-A57\n0xd08\nCortex-A72\n\
0xd09\nCortex-A73\n0xd0a\nCortex-A75\n0xd0b\nCortex-A76\n0xd0c\nNeoverse-N1\n0xd0d\nCortex-A77\n\
0xd0e\nCortex-A76AE\n0xd13\nCortex-R52\n0xd20\nCortex-M23\n0xd21\nCortex-M33\n0xd41\nCortex-A78\n\
0xd42\nCortex-A78AE\n0xd4a\nNeoverse-E1\n0xd4b\nCortex-A78C\n";
/// The part table of the build's target: AArch64's, empty elsewhere.
#[cfg(target_arch = "aarch64")]
const PARTS: &[u8] = AARCH64_PARTS;
#[cfg(not(target_arch = "aarch64"))]
const PARTS: &[u8] = b"";

/// A C stdio stream over a file's bytes, for `uv_cpu_info`'s `fscanf` and
/// `fgets`.
struct Stream<'a> {
    b: &'a [u8],
    i: usize,
}

impl Stream<'_> {
    /// `fgets(buf, 1024, fp)`: up to 1023 bytes, through the first newline.
    fn fgets(&mut self) -> Option<&[u8]> {
        if self.i >= self.b.len() {
            return None;
        }
        let rest = &self.b[self.i..];
        let n = rest
            .iter()
            .position(|&c| c == b'\n')
            .map_or(rest.len(), |k| k + 1)
            .min(1023);
        self.i += n;
        Some(&rest[..n])
    }

    fn skip_space(&mut self) {
        while self.i < self.b.len() && is_c_space(self.b[self.i]) {
            self.i += 1;
        }
    }

    fn literal(&mut self, lit: &[u8]) -> bool {
        if self.b[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn unsigned(&mut self) -> Option<u64> {
        scan_unsigned(self.b, &mut self.i)
    }
}

/// `cpuInfo` (`uv_cpu_info` on Linux): one entry per `cpuN` line of
/// `/proc/stat` in CPU order, its times in ticks (100 per second) times 10;
/// the model from `/proc/cpuinfo`'s marker line (on AArch64 the CPU part
/// code looked up in libuv's table, `unknown` when absent), matched against
/// at most eight model slots as libuv matches them (by prefix, the slot after
/// the matching one taking the name); the speed `scaling_cur_freq` in MHz, 0
/// without the file. `/proc/stat` that cannot be opened is its error; where
/// libuv aborts (a `/proc/stat` without a line, a frequency file without a
/// number), the result is `UV_ENOENT`'s error instead.
pub fn cpu_info() -> Result<Vec<CpuInfo>, IoError> {
    let stat = std::fs::read("/proc/stat").map_err(|e| std_err(&e))?;
    let info = std::fs::read("/proc/cpuinfo").map_err(|e| failed(&e)).ok();
    parse_cpu_info(&stat, info.as_deref(), MODEL_MARKER, PARTS, |cpu| {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq");
        std::fs::read(path).map_err(|e| failed(&e)).ok()
    })
}

/// [`cpu_info`] over the files' bytes: `/proc/stat`, `/proc/cpuinfo` (when
/// it could be read), the target's model marker and part table, and each
/// CPU's `scaling_cur_freq` (`None` when it could not be read).
fn parse_cpu_info(
    stat: &[u8],
    cpuinfo: Option<&[u8]>,
    marker: &[u8],
    parts: &[u8],
    mut freq: impl FnMut(usize) -> Option<Vec<u8>>,
) -> Result<Vec<CpuInfo>, IoError> {
    const MAX_CPUS: usize = 8192;
    let mut st = Stream { b: stat, i: 0 };
    if st.fgets().is_none() {
        return Err(libuv(ENOENT));
    }
    let mut cpus: std::collections::BTreeMap<usize, (u64, CpuTimes, usize)> =
        std::collections::BTreeMap::new();
    let mut maxcpu = 0usize;
    loop {
        if !st.literal(b"cpu") {
            break;
        }
        let Some(cpu) = st.unsigned() else { break };
        let mut v = [0u64; 6];
        let mut ok = true;
        for x in &mut v {
            st.skip_space();
            match st.unsigned() {
                Some(n) => *x = n,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            break;
        }
        if st.fgets().is_none() {
            return Err(libuv(ENOENT));
        }
        let cpu = cpu as u32 as usize;
        if cpu >= MAX_CPUS {
            continue;
        }
        let times = CpuTimes {
            user: v[0],
            nice: v[1],
            sys: v[2],
            idle: v[3],
            irq: v[5],
        };
        cpus.insert(cpu, (0, times, 0));
        if cpu >= maxcpu {
            maxcpu = cpu + 1;
        }
    }
    let mut models: Vec<Vec<u8>> = vec![Vec::new(); 8];
    models[0] = b"unknown".to_vec();
    if let Some(info) = cpuinfo {
        let mut f = Stream { b: info, i: 0 };
        loop {
            // fscanf(fp, "processor\t: %u\n", &cpu)
            if !f.literal(b"processor") {
                break;
            }
            f.skip_space();
            if !f.literal(b":") {
                break;
            }
            f.skip_space();
            let Some(cpu) = f.unsigned() else { break };
            f.skip_space();
            let cpu = cpu as u32 as usize;
            let mut line: Option<Vec<u8>> = None;
            while let Some(l) = f.fgets() {
                if l.starts_with(marker) {
                    line = Some(l.to_vec());
                    break;
                }
            }
            if let Some(buf) = line {
                let p0 = &buf[marker.len()..];
                let n0 = line_len(p0);
                let name: Vec<u8> = if parts.is_empty() {
                    p0[..n0].to_vec()
                } else {
                    // memmem(parts, ..., p, n + 1): the code and the byte after it
                    let mut key = p0[..n0].to_vec();
                    key.push(p0.get(n0).copied().unwrap_or(0));
                    match find(parts, &key) {
                        Some(at) => {
                            let p = &parts[at + key.len()..];
                            p[..line_len(p)].to_vec()
                        }
                        None => b"unknown".to_vec(),
                    }
                };
                // libuv's slot loop: `model` ends one past the first slot whose
                // name prefixes `name`
                let matched = models
                    .iter()
                    .position(|m| name.len() >= m.len() && name[..m.len()] == m[..]);
                if let Some(k) = matched {
                    let slot = k + 1;
                    if slot < models.len() {
                        if models[slot].is_empty() {
                            let mut m = name.clone();
                            m.truncate(63);
                            models[slot] = m;
                        }
                        if cpu < maxcpu {
                            if let Some(c) = cpus.get_mut(&cpu) {
                                c.2 = slot;
                            }
                        }
                    }
                }
            }
            // to the blank line that ends this processor's block
            while let Some(l) = f.fgets() {
                if l.first() == Some(&b'\n') {
                    break;
                }
            }
        }
    }
    for (cpu, c) in cpus.iter_mut() {
        let Some(text) = freq(*cpu) else {
            continue;
        };
        let mut i = 0;
        match scan_unsigned(&text, &mut i) {
            Some(freq) => c.0 = freq,
            None => return Err(libuv(ENOENT)),
        }
    }
    Ok(cpus
        .values()
        .map(|&(freq, t, model)| CpuInfo {
            model: models[model].clone(),
            speed: freq / 1000,
            times: CpuTimes {
                user: t.user.wrapping_mul(10),
                nice: t.nice.wrapping_mul(10),
                sys: t.sys.wrapping_mul(10),
                idle: t.idle.wrapping_mul(10),
                irq: t.irq.wrapping_mul(10),
            },
        })
        .collect())
}

#[cfg(test)]
#[path = "uvsys_tests.rs"]
mod tests;
