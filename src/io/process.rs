//! Child processes: `IO.Process.spawn` and the `Child` operations, following
//! the POSIX branch of Lean 4.34.0's `src/runtime/process.cpp`, and
//! `IO.Process.output`, whose Lean definition reads the child's standard
//! output on a dedicated task.
//!
//! Lean forks and the child runs `execvp`. This crate cannot fork in safe
//! Rust, so it spawns through `posix_spawn` (`nix`'s safe wrapper) and
//! reproduces what the forked child does before `execvp`:
//!
//! 1. **Standard streams.** `piped` is a pipe (`pipe2` with `O_CLOEXEC`)
//!    whose parent end becomes a [`Handle`] (`fdopen` `"w"` for standard
//!    input, `"r"` for the other two), `inherit` keeps the parent's
//!    descriptor, `null` is `/dev/null`. A field whose stream is not `piped`
//!    is Lean's `()`, here `None`. When standard input is `inherit`, standard
//!    output is flushed first (`std::cout.flush()`).
//! 2. **Program and arguments.** `cmd` and each argument are C strings (cut
//!    at their first NUL byte), `argv[0]` is `cmd`, and the program is found
//!    as glibc 2.39's `execvp` finds it ([`Spec::exec_search`]), the search
//!    running over `posix_spawn`, a file the kernel refuses with `ENOEXEC`
//!    included (run by `/bin/sh`).
//! 3. **Environment.** The child's `envp` is the parent's `environ`
//!    ([`super::environ`], every entry in order) or nothing when
//!    `inheritEnv` is false (`clearenv`), with `env` applied in order as
//!    glibc's `setenv` and `unsetenv` apply it; the parent's own environment
//!    never changes.
//! 4. **Working directory.** Lean's child calls `chdir(cwd)`. Here a
//!    long-lived spawner thread, which has unshared its file-system
//!    attributes from the process (`unshare(CLONE_FS)`, `nix`'s safe
//!    wrapper), enters `cwd` and spawns; `posix_spawn`'s child copies that
//!    thread's working directory. A relative `cwd` is entered from the
//!    caller's working directory (an `O_PATH` descriptor of `.` the caller
//!    opens, then `fchdir` and `chdir`; its path where no descriptor is
//!    left), so it fails exactly when the forked child's `chdir` would: when
//!    the process may not search its own working directory, or that
//!    directory has been removed. The process's working directory never
//!    changes, and an absolute `cwd` works from a working directory the
//!    process may not search, as natively (this closes leanrs's DV15 (d)).
//!    Relative `PATH` entries and programs resolve against `cwd`, as after
//!    Lean's `chdir`. The child gets the caller's attributes that Linux
//!    keeps per thread: its signal mask (`POSIX_SPAWN_SETSIGMASK` with the
//!    caller's mask, for every spawn) and its nice value, which the spawner
//!    takes before each spawn; where it cannot (lowering a nice value needs
//!    privilege), the caller makes that spawn on a short-lived helper thread
//!    of its own, which has the caller's nice value and unshares its
//!    file-system attributes in turn (review RIO2-14). Both go back to `/`
//!    after the spawn, the helper before it ends, so no thread of the
//!    process holds `cwd` once the caller has the result (leanrs review
//!    LRIO2-F1: `lsof` and `umount` would see it; a dying thread's working
//!    directory is dropped only after `join` returns). The spawner's
//!    file-system attributes are a copy taken at its `unshare`: its `umask`
//!    and root directory stay the process's of that moment, so a child
//!    spawned with a `cwd` after a `umask` change (by user C code: Lean has
//!    no `umask`) gets the old mask. The child's parent thread is the
//!    spawner, which lives as long as the process (so `PR_SET_PDEATHSIG`
//!    fires at the process's exit, natively at the calling thread's).
//!
//!    **Fallback.** Where `unshare(CLONE_FS)` is refused (Docker's default
//!    seccomp profile gives `EPERM` without `CAP_SYS_ADMIN`; `ENOSYS`,
//!    `EINVAL`), the calling thread enters `cwd` for the process, spawns,
//!    and returns by `fchdir` to an `O_PATH` descriptor of the directory it
//!    left. Meanwhile it holds a lock that the runtime's other changes of
//!    the working directory (`setCurrentDir`, `uv_chdir`) take exclusively,
//!    and its reads of it (`IO.currentDir`, `IO.Process.getCurrentDir`,
//!    `uv_cwd`) and spawns without a `cwd` take shared, so none of them runs
//!    while the process is in `cwd` (reviews RIO2-13, RIO2-20). Limits
//!    remain there:
//!    - another thread's path operation during the spawn (an open, a
//!      metadata query, a directory listing, a removal: every call that
//!      takes a path relative to the working directory, or one through
//!      `/proc/self/cwd`) still sees `cwd`, in a build without the feature
//!      `threads`. The single-thread scheduler runs Lean code on one thread,
//!      so there only a translator's own threads can do that. In threads
//!      mode every task can, so there the lookups of every path the program
//!      supplies hold the lock shared ([`with_path_lookup`],
//!      [`open_looked_up`]) while a fallback spawn may happen
//!      (docs/threads.md, 3.2; review RT1-04): see "Threads mode" below;
//!    - the way back, checked before leaving, fails only if another process
//!      changes the directory's mode (or moves it, where the way back is a
//!      path), and the process then stays in `cwd`;
//!    - a process that may not search its own working directory cannot come
//!      back, so an absolute `cwd` is then the spawn's `EACCES` (leanrs's
//!      DV15 (d)); a relative one fails as natively.
//!
//!    The real fix, for later, is a spawn that needs no change of the
//!    process's working directory: glibc 2.29's
//!    `posix_spawn_file_actions_addchdir_np` (POSIX 2024's
//!    `posix_spawn_file_actions_addchdir`), which `nix` does not wrap yet and
//!    this crate cannot call without `unsafe`.
//!
//!    **Threads mode** (feature `threads`; docs/threads.md, 3.2). `sched::start`
//!    decides the spawn path before any task runs: it starts the spawner
//!    thread ([`decide_spawn_path`]).
//!    - If the spawner unshares its file-system attributes (the common
//!      case), `NO_FALLBACK` is set, under `CWD_LOCK` held exclusively. From
//!      then on no spawn takes the fallback: `fallback_spawn` refuses one
//!      with `EAGAIN` (only a later refusal of `unshare`, after a first
//!      success, gets there). So lookups take no lock.
//!    - Otherwise every lookup of a path the program supplies holds
//!      `CWD_LOCK` shared for the rest of the run ([`with_path_lookup`],
//!      [`open_looked_up`]), so a fallback spawn's stay in `cwd` excludes
//!      them, as it excludes `currentDir`. One uncontended read lock per
//!      call.
//!    - Such paths take it when absolute too. An absolute path is resolved
//!      from the root directory, which `chdir` does not change, but it can
//!      still reach the working directory: through `/proc/self/cwd` (a
//!      magic link to the process's working directory; also
//!      `/proc/thread-self/cwd` and `/proc/<pid>/cwd`), directly or through
//!      any symbolic link whose target goes there. So no path the program
//!      supplies is safe to look up without the lock while the process may
//!      be in another thread's `cwd`. The paths the crate names itself
//!      (`/proc/self/exe`, `/proc/self/environ`, `/proc/self/fd/N`,
//!      `/dev/urandom`, `/dev/null`, `/bin/sh`) take no lock: none goes
//!      through the working directory (review RT2-L-03).
//!
//!    **No wait under `CWD_LOCK`** (review RT2-03). A fallback spawn waits for
//!    every reader of `CWD_LOCK`, and while it waits std's lock lets no new
//!    reader in. So no holder of it waits for another thread or process:
//!    its readers are lookups that never wait (an `open(2)` of a path the
//!    program supplies is made under the lock only with `O_NONBLOCK`, or as
//!    an `O_PATH` resolution, the file then opened after the unlock,
//!    [`open_looked_up`]), `getcwd`, and `posix_spawn`, which
//!    returns once the child has called `execve` (spawns without a `cwd`,
//!    the stand-in); its writers are a `chdir`, a fallback spawn (a
//!    `posix_spawn` too) and the spawner's first success (`NO_FALLBACK`), so
//!    the spawner's wait for the write lock ends too.
//!
//!    **Lock order.** `SPAWNER`, then `CWD_LOCK`: `spawner` sets
//!    `NO_FALLBACK` under both; no holder of `CWD_LOCK` takes `SPAWNER`.
//!    `CWD_LOCK` is a leaf otherwise: under it the crate makes only system
//!    calls (and a spawn, which takes no other lock of the crate), never a
//!    stream lock, the scheduler's lock or `sched::uv`'s loop lock, and runs
//!    no translator code (a caller's sink is filled after the unlock). It
//!    may be taken while those are held (a glue that holds a stream's guard,
//!    a `sync` dependent that runs under the loop lock and opens a file),
//!    never the other way, so no cycle goes through it. It is never taken
//!    twice by one thread ([`with_path_lookup`] is never called with it
//!    held: a second read lock waits behind a waiting writer). The
//!    scheduler's lock is never held across io (no translator code runs
//!    under it), so it is never held with `CWD_LOCK` either.
//! 5. **`setsid`** is `POSIX_SPAWN_SETSID`. `posix_spawn` resets no signal
//!    disposition, so an ignored `SIGPIPE` stays ignored, as across `fork`.
//! 6. **A child that cannot start.** Lean's `spawn` succeeds even when `cwd`
//!    cannot be entered or the program cannot be executed: the forked child
//!    writes `could not change directory to <cwd>` or `could not execute
//!    external process '<cmd>'` and a newline to its standard error, and
//!    exits with status 255. Natively it first writes its copy of the
//!    parent's pending standard-output bytes (`std::cerr` is tied to
//!    `std::cout`, so the message flushes the buffer the fork copied), which
//!    the parent writes again later; the runtime does not copy that (LB-42,
//!    `docs/lean-bugs.md`). `posix_spawn` reports both failures
//!    to the parent instead, and the runtime starts a stand-in, a `/bin/sh`
//!    with the child's standard streams, in a new session when the program
//!    failed and `setsid` was asked for (Lean's child calls `setsid()` after
//!    `chdir`, before `execvp`). [`STAND_IN_LIFE`] after the spawn (about the
//!    forked child's delay), the stand-in itself writes the message (its
//!    argument, by `printf`), and exits with 255. So the parent's next
//!    lines come first, as natively. The child is a real process, with a pid,
//!    `wait`, `kill` and `killpg`, and a standard input that closes when it
//!    exits, as natively. Where the stand-in cannot be set up (no `/bin/sh`,
//!    or no descriptor left for its pipes: `EMFILE`, which natively leaves
//!    the forked child running), the runtime models that child: it writes the
//!    message at once; the pid is above any the kernel
//!    gives (counting down from `0x7FFFFFFF`); `wait` gives 255 once; `kill`
//!    succeeds until it has been waited (`killpg` of a child that failed at
//!    `chdir` finding no group); and a piped standard input takes a pipe's
//!    capacity and then fails with `EPIPE`.
//! 7. **Waiting and signals.** `wait` and `tryWait` are `waitpid`, the status
//!    or 128 plus the signal (bash's convention); `kill` is `SIGKILL`, to the
//!    process group (`killpg`) for a child spawned with `setsid`.
//!    `takeStdin` returns the same process with its `setsid` flag. Lean's
//!    builds a child object without the flag, so `kill` after it signals the
//!    pid alone and the group survives (LB-14, `docs/lean-bugs.md`); the
//!    runtime keeps the flag, as `Child.kill`'s documentation promises.
//!
//! Errors of the parent's own calls (a pipe, a thread, `posix_spawn` itself)
//! are `decode_io_error(errno, nullptr)`, as Lean's `throw errno`; a thread
//! the system cannot create is `EAGAIN`, as `fork`'s failure is.
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/process.rs` (docs ch05 O8, D25;
//! probes `validate/io/proc_spawn`, `proc_output`, `proc_inherit`, A755),
//! rewritten over this crate's handles and views; lean2rr's
//! `runtime/leanrt/src/proc.rs` (the fork-based original, which gave the
//! native outputs every case here was checked against) and its `drain`
//! (both pipes of `output` read on the calling thread with `poll`).

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError, RwLock};

use nix::errno::Errno;
use nix::spawn::{posix_spawn, PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags};
use nix::sys::signal::SigSet;
use rustix::fd::{AsFd, AsRawFd, OwnedFd};
use rustix::process::{Pid, Signal, WaitOptions};

use super::error::{
    set_errno, IoError, E2BIG, EACCES, EAGAIN, ECHILD, EINVAL, EIO, EISDIR, ELOOP, EMFILE,
    ENAMETOOLONG, ENFILE, ENODEV, ENOENT, ENOEXEC, ENOMEM, ENOTDIR, EPERM, EPIPE, ESRCH, ETIMEDOUT,
    ETXTBSY,
};
use super::handle::{FsMode, Handle};
use super::{environ, ByteSink};

/// `ESTALE` (not among the crate's constants).
const ESTALE: i32 = 116;
/// `ELIBBAD`: `execve` of a corrupt shared-library interpreter.
const ELIBBAD: i32 = 80;
/// glibc's `NAME_MAX`, the longest program name `execvp` searches for.
const NAME_MAX: usize = 255;
/// glibc's `PATH_MAX`.
const PATH_MAX: usize = 4096;
/// POSIX's `PIPE_BUF`: a pipe always holds at least this many bytes.
const PIPE_BUF: usize = 4096;
/// glibc's `POSIX_SPAWN_SETSID` (`spawn.h`, glibc 2.26), which `nix`'s
/// `PosixSpawnFlags` does not name.
const POSIX_SPAWN_SETSID: i32 = 0x80;
/// The size of the buffer `output` reads the child's pipes into.
const READ_CHUNK: usize = 65536;

/// Lean's `IO.Process.Stdio`, its constructors in Lean's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stdio {
    Piped,
    Inherit,
    Null,
}

impl Stdio {
    /// The mode of Lean's constructor index (0 to 2).
    pub fn from_index(i: u8) -> Option<Stdio> {
        Some(match i {
            0 => Stdio::Piped,
            1 => Stdio::Inherit,
            2 => Stdio::Null,
            _ => return None,
        })
    }
}

/// Lean's `IO.Process.StdioConfig`, its fields in Lean's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StdioConfig {
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

/// The fields of Lean's `IO.Process.SpawnArgs` other than its
/// `StdioConfig`, as views of the program's strings.
#[derive(Clone, Copy, Debug)]
pub struct SpawnArgs<'a> {
    pub cmd: &'a [u8],
    pub args: &'a [&'a [u8]],
    pub cwd: Option<&'a [u8]>,
    /// The changes, in order: `Some(v)` sets the variable, `None` unsets it.
    pub env: &'a [(&'a [u8], Option<&'a [u8]>)],
    pub inherit_env: bool,
    pub setsid: bool,
}

/// What `IO.Process.spawn` returns: Lean's `Child` object, the three stream
/// fields (`None` for a stream that is not `piped`, Lean's `()`) and the
/// process. A translator keeps the handles in its own fields and the
/// [`ChildProcess`] beside them.
#[derive(Debug)]
pub struct Child {
    pub stdin: Option<Handle>,
    pub stdout: Option<Handle>,
    pub stderr: Option<Handle>,
    pub process: ChildProcess,
}

/// The process of a `Child`: its pid and `setsid` flag, as in Lean's object,
/// and, for the modelled child of module comment item 6 (no `/bin/sh`), its
/// state, which clones (and the child `takeStdin` returns) share.
#[derive(Clone, Debug)]
pub struct ChildProcess {
    pid: u32,
    setsid: bool,
    modelled: Option<Arc<Mutex<Modelled>>>,
}

/// The state of a modelled child that could not start (module comment,
/// item 6, when `/bin/sh` cannot be spawned).
#[derive(Debug)]
struct Modelled {
    /// The read end of its piped standard input, held until it is waited.
    stdin_reader: Option<OwnedFd>,
    reaped: bool,
    /// It failed at `chdir`, before Lean's child calls `setsid()`.
    at_chdir: bool,
}

/// Why a child cannot start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Cwd,
    Program,
}

/// A failure of the spawn: the child's own (it starts and fails, status
/// 255), or the parent's (`errno`), which `spawn` reports.
#[derive(Debug)]
enum SpawnError {
    Child(Failure),
    Os(i32),
}

/// `decode_io_error(errno, nullptr)` of a failing call, with the modelled
/// `errno` set as the call sets C's.
fn os_error(e: i32) -> IoError {
    set_errno(e);
    IoError::decode_io_error(e, None)
}

/// The `errno` of a thread the system could not create (`pthread_create`'s
/// `EAGAIN`, as `fork`'s).
fn thread_error(e: std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EAGAIN)
}

/// A C string's bytes: up to the first NUL byte (Lean passes `cmd`, the
/// arguments, `cwd` and the environment to the system as C strings).
fn c_text(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(n) => &s[..n],
        None => s,
    }
}

fn c_string(s: &[u8]) -> CString {
    // `c_text` leaves no NUL byte
    CString::new(c_text(s)).unwrap_or_default()
}

/// A `nix` pid as `rustix`'s (positive).
fn to_pid(p: nix::unistd::Pid) -> Result<Pid, i32> {
    Pid::from_raw(p.as_raw()).ok_or(EAGAIN)
}

/// A spawn's program, arguments, working directory, environment and session
/// flag as the C strings Lean passes.
struct Spec {
    cmd: Vec<u8>,
    argv: Vec<CString>,
    cwd: Option<Vec<u8>>,
    /// The child's environment (shared with the runtime's cached one when it
    /// is inherited unchanged).
    envp: Arc<[CString]>,
    setsid: bool,
    /// The calling thread's signal mask, the child's (a forked child keeps
    /// the forking thread's), whichever thread spawns.
    sigmask: Option<SigSet>,
}

impl Spec {
    fn new(a: &SpawnArgs) -> Spec {
        let cmd = c_text(a.cmd).to_vec();
        let mut argv = Vec::with_capacity(a.args.len() + 1);
        argv.push(c_string(&cmd));
        argv.extend(a.args.iter().map(|s| c_string(s)));
        let envp = if a.inherit_env && a.env.is_empty() {
            environ::envp()
        } else {
            let mut env = if a.inherit_env {
                environ::entries()
            } else {
                Vec::new()
            };
            for (name, value) in a.env {
                let name = c_text(name);
                // `setenv` and `unsetenv` refuse such a name and change nothing
                if !environ::name_ok(name) {
                    continue;
                }
                match value {
                    Some(v) => environ::apply_set(&mut env, name, c_text(v)),
                    None => environ::apply_unset(&mut env, name),
                }
            }
            env.iter()
                .filter_map(|e| CString::new(e.as_slice()).ok())
                .collect()
        };
        Spec {
            cmd,
            argv,
            cwd: a.cwd.map(|d| c_text(d).to_vec()),
            envp,
            setsid: a.setsid,
            sigmask: SigSet::thread_get_mask().ok(),
        }
    }

    /// The message Lean's child writes when it cannot start.
    fn failure_message(&self, f: Failure) -> Vec<u8> {
        let mut m = Vec::new();
        match (f, &self.cwd) {
            (Failure::Cwd, Some(d)) => {
                m.extend_from_slice(b"could not change directory to ");
                m.extend_from_slice(d);
            }
            _ => {
                m.extend_from_slice(b"could not execute external process '");
                m.extend_from_slice(&self.cmd);
                m.push(b'\'');
            }
        }
        m.push(b'\n');
        m
    }

    /// `posix_spawn` with the standard streams `dups` (each a descriptor and
    /// its target 0, 1 or 2) and `setsid`, through glibc's `execvp` search,
    /// from the calling thread's working directory.
    fn spawn_here(&self, dups: &[(i32, i32)]) -> Result<Pid, SpawnError> {
        let os = |e: Errno| SpawnError::Os(e as i32);
        let mut actions = PosixSpawnFileActions::init().map_err(os)?;
        for &(fd, target) in dups {
            actions.add_dup2(fd, target).map_err(os)?;
        }
        let mut attr = PosixSpawnAttr::init().map_err(os)?;
        let mut flags = PosixSpawnFlags::empty();
        if self.setsid {
            flags |= PosixSpawnFlags::from_bits_retain(POSIX_SPAWN_SETSID);
        }
        if let Some(mask) = &self.sigmask {
            attr.set_sigmask(mask).map_err(os)?;
            flags |= PosixSpawnFlags::POSIX_SPAWN_SETSIGMASK;
        }
        if !flags.is_empty() {
            attr.set_flags(flags).map_err(os)?;
        }
        self.exec_search(&actions, &attr).map_err(|e| {
            if is_exec_failure(e) {
                SpawnError::Child(Failure::Program)
            } else {
                SpawnError::Os(e)
            }
        })
    }

    /// One `execve` of `file` (with `argv` and `envp`) as `posix_spawn`, and
    /// glibc's `maybe_script_execute` after `ENOEXEC`: `/bin/sh file args`.
    fn exec_one(
        &self,
        file: &[u8],
        actions: &PosixSpawnFileActions,
        attr: &PosixSpawnAttr,
    ) -> Result<Pid, i32> {
        match posix_spawn(file, actions, attr, &self.argv, &self.envp) {
            Ok(p) => to_pid(p),
            Err(Errno::ENOEXEC) => {
                let mut sh = Vec::with_capacity(self.argv.len() + 1);
                sh.push(CString::from(c"/bin/sh"));
                sh.push(CString::new(file).unwrap_or_default());
                sh.extend(self.argv.iter().skip(1).cloned());
                posix_spawn(c"/bin/sh", actions, attr, &sh, &self.envp)
                    .map_err(|e| e as i32)
                    .and_then(to_pid)
            }
            Err(e) => Err(e as i32),
        }
    }

    /// glibc 2.39's `__execvpe_common` over [`Spec::exec_one`]: an empty name
    /// is `ENOENT`; a name holding `/` is tried as it is; a name longer than
    /// `NAME_MAX` is `ENAMETOOLONG`; otherwise each entry of the child's
    /// `PATH` (its first `PATH=` entry, `/bin:/usr/bin` without one; an
    /// empty entry the working directory; an entry as long as the whole,
    /// `PATH_MAX`-capped value skipped) joined with the name, in order, going
    /// on after `EACCES` (remembered), `ENOENT`, `ESTALE`, `ENOTDIR`, `ENODEV`
    /// and `ETIMEDOUT` and stopping at any other error, `EACCES` when some
    /// entry gave it.
    fn exec_search(
        &self,
        actions: &PosixSpawnFileActions,
        attr: &PosixSpawnAttr,
    ) -> Result<Pid, i32> {
        let file = self.cmd.as_slice();
        if file.is_empty() {
            return Err(ENOENT);
        }
        if file.contains(&b'/') {
            return self.exec_one(file, actions, attr);
        }
        if file.len() > NAME_MAX {
            return Err(ENAMETOOLONG);
        }
        let path: &[u8] = self
            .envp
            .iter()
            .find_map(|e| e.to_bytes().strip_prefix(b"PATH="))
            .unwrap_or(b"/bin:/usr/bin");
        let path_len = path.len().min(PATH_MAX - 1) + 1;
        let mut got_eacces = false;
        let mut buf = Vec::with_capacity(path_len + file.len() + 1);
        let mut p = 0;
        loop {
            let subp = p + path[p..]
                .iter()
                .position(|&c| c == b':')
                .unwrap_or(path.len() - p);
            if subp - p >= path_len {
                // glibc's `continue` leaves `p` at the `:`: the next entry is empty
                if subp == path.len() {
                    break;
                }
                p = subp;
                continue;
            }
            buf.clear();
            buf.extend_from_slice(&path[p..subp]);
            if subp > p {
                buf.push(b'/');
            }
            buf.extend_from_slice(file);
            if missing(&buf) {
                // `execve` would fail as the lookup does, and the search goes on
                if subp == path.len() {
                    break;
                }
                p = subp + 1;
                continue;
            }
            match self.exec_one(&buf, actions, attr) {
                Ok(pid) => return Ok(pid),
                Err(EACCES) => got_eacces = true,
                Err(ENOENT | ESTALE | ENOTDIR | ENODEV | ETIMEDOUT) => {}
                Err(e) => return Err(e),
            }
            if subp == path.len() {
                break;
            }
            p = subp + 1;
        }
        Err(if got_eacces { EACCES } else { ENOENT })
    }
}
/// Whether `file` does not exist as a path lookup with the effective ids
/// sees it (`faccessat(AT_FDCWD, file, F_OK, AT_EACCESS)` failing with
/// `ENOENT` or `ENOTDIR`): its `execve` would fail with that same error, which
/// the search passes over. Checking first spares a `posix_spawn` (a clone of
/// the process) per `PATH` entry that does not hold the program, where Lean's
/// child pays a failing `execve` only. Any other answer, an existing file
/// included, leaves the file to `execve`.
fn missing(file: &[u8]) -> bool {
    matches!(
        rustix::fs::accessat(
            rustix::fs::CWD,
            file,
            rustix::fs::Access::EXISTS,
            rustix::fs::AtFlags::EACCESS
        ),
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR)
    )
}

/// Whether an error of the search is `execve`'s, the program's failure that
/// Lean's forked child reports, rather than the spawn's own (`posix_spawn`
/// reports both): every error `execve` gives for a file, those the search
/// passes over included, since the last entry's error ends the search.
fn is_exec_failure(e: i32) -> bool {
    matches!(
        e,
        ENOENT
            | EACCES
            | ENOEXEC
            | ENOTDIR
            | ELOOP
            | ENAMETOOLONG
            | EPERM
            | ETXTBSY
            | EISDIR
            | E2BIG
            | EINVAL
            | EIO
            | ELIBBAD
            | EMFILE
            | ENFILE
            | ESTALE
            | ENODEV
            | ETIMEDOUT
    )
}

/// `pipe2(fds, O_CLOEXEC)`: the read and write ends.
fn pipe() -> Result<(OwnedFd, OwnedFd), i32> {
    match std::io::pipe() {
        Ok((r, w)) => Ok((OwnedFd::from(r), OwnedFd::from(w))),
        Err(e) => Err(e.raw_os_error().unwrap_or(EAGAIN)),
    }
}

/// `open(".", O_PATH | O_DIRECTORY | O_CLOEXEC)`: the calling thread's
/// working directory, which it can come back to (`fchdir`) or enter from
/// another thread. It fails when the process may not search that directory.
fn open_dot() -> Result<OwnedFd, rustix::io::Errno> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(
        ".",
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

// ---- spawns with a `cwd` (module comment, item 4) ----

/// A spawn the spawner thread makes for a caller, who waits for the answer.
struct Job {
    spec: Arc<Spec>,
    /// The child's standard streams (descriptors and their targets), which
    /// the caller keeps open until the answer.
    dups: Vec<(i32, i32)>,
    /// For a relative `cwd`: the caller's working directory.
    base: Base,
    /// The caller thread's nice value.
    nice: Option<i32>,
    answer: mpsc::SyncSender<Answer>,
}

/// The spawner's answer to a [`Job`].
enum Answer {
    Spawned(Result<Pid, SpawnError>),
    /// The spawner could not take the caller's nice value (lowering a
    /// thread's nice value needs privilege): the caller runs the job on a
    /// helper thread of its own ([`on_helper`]).
    Refused(Job),
}

/// The working directory a relative `cwd` starts from: the caller's, as an
/// `O_PATH` descriptor, or as its path where no descriptor is left
/// (`EMFILE`, `ENFILE`, `ENOMEM`: natively the forked child's `chdir` needs
/// none); or none, with the error, where the forked child's `chdir` of a
/// relative path fails as well: the process may not search the directory
/// (`EACCES`) or it has been removed (`ENOENT`).
enum Base {
    Fd(OwnedFd),
    Path(std::path::PathBuf),
    Fails(rustix::io::Errno),
    /// The `cwd` is absolute.
    Unused,
}

impl Base {
    /// The calling thread's working directory, for a relative `cwd`. An
    /// error is the parent's: no descriptor left, and the path not found
    /// either (LRIO2-F3, RIO2-16).
    fn here() -> Result<Base, i32> {
        use rustix::io::Errno as E;
        match open_dot() {
            Ok(f) => Ok(Base::Fd(f)),
            Err(e @ (E::ACCESS | E::NOENT)) => Ok(Base::Fails(e)),
            Err(e) => match std::env::current_dir() {
                Ok(p) => Ok(Base::Path(p)),
                Err(d) if d.raw_os_error() == Some(ENOENT) => Ok(Base::Fails(E::NOENT)),
                Err(_) => Err(e.raw_os_error()),
            },
        }
    }

    /// Makes it the calling thread's working directory.
    fn enter(&self) -> Result<(), rustix::io::Errno> {
        match self {
            Base::Fd(f) => rustix::process::fchdir(f),
            Base::Path(p) => rustix::process::chdir(p.as_path()),
            Base::Fails(e) => Err(*e),
            Base::Unused => Ok(()),
        }
    }
}

impl Job {
    /// On the spawner thread: take the caller's nice value (or hand the job
    /// back), enter `cwd`, spawn, and go back to `/`, so the spawner holds
    /// no directory (`/proc/<pid>/task/*/cwd`, `lsof` and `umount` would see
    /// it).
    fn run(self) {
        if let Some(n) = self.nice {
            if rustix::process::setpriority_process(None, n).is_err() {
                let answer = self.answer.clone();
                let _ = answer.send(Answer::Refused(self));
                return;
            }
        }
        let r = self.spawn_and_leave();
        let _ = self.answer.send(Answer::Spawned(r));
    }

    /// `spawn`, then back to `/`, so this thread holds no directory once the
    /// caller has the result (LRIO2-F1). The spawner thread does it before
    /// its answer; a helper thread (`on_helper`) before it ends, since its
    /// end is not enough: `join` returns before the kernel drops a dying
    /// thread's working directory, which the thread then still held (the
    /// test `spawner_leaves_cwd` failed under load).
    fn spawn_and_leave(&self) -> Result<Pid, SpawnError> {
        let r = self.spawn();
        let _ = rustix::process::chdir("/");
        r
    }

    /// Enters `cwd` (a relative one from the caller's working directory)
    /// and spawns, on a thread with file-system attributes of its own.
    fn spawn(&self) -> Result<Pid, SpawnError> {
        let cwd = self.spec.cwd.as_deref().unwrap_or_default();
        let cannot = |_| SpawnError::Child(Failure::Cwd);
        self.base.enter().map_err(cannot)?;
        rustix::process::chdir(cwd).map_err(cannot)?;
        self.spec.spawn_here(&self.dups)
    }
}

/// The spawner thread's queue, once it runs.
static SPAWNER: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);
/// `unshare(CLONE_FS)` was refused: spawns with a `cwd` take the fallback.
static NO_PRIVATE_CWD: AtomicBool = AtomicBool::new(false);
/// Test hook: spawns with a `cwd` take the fallback.
#[cfg(test)]
pub(crate) static FORCE_FALLBACK: AtomicBool = AtomicBool::new(false);
/// Test hook: `/bin/sh` cannot be spawned, so a child that cannot start is
/// modelled.
#[cfg(test)]
pub(crate) static FORCE_NO_SHELL: AtomicBool = AtomicBool::new(false);
/// The process's working directory as the runtime changes it: the
/// fallback's spawns hold it exclusively while the process is in their
/// `cwd`, [`with_cwd_change`] (`setCurrentDir`, `uv_chdir`) exclusively,
/// [`with_cwd_read`] (`currentDir`, `getCurrentDir`, `uv_cwd`) and spawns
/// without a `cwd` shared (module comment, item 4; reviews RIO2-13,
/// RIO2-20); in threads mode also path operations ([`with_path_lookup`])
/// while a fallback spawn may happen (review RT1-04).
static CWD_LOCK: RwLock<()> = RwLock::new(());

/// Runs `f`, a change of the process's working directory, never during a
/// fallback spawn's (which would come back over it) nor during a spawn
/// without a `cwd`.
pub(crate) fn with_cwd_change<T>(f: impl FnOnce() -> T) -> T {
    let _g = CWD_LOCK.write().unwrap_or_else(PoisonError::into_inner);
    f()
}

/// Runs `f`, a read of the process's working directory, never during a
/// fallback spawn's (it would read the spawn's `cwd`) nor during a change.
pub(crate) fn with_cwd_read<T>(f: impl FnOnce() -> T) -> T {
    let _g = CWD_LOCK.read().unwrap_or_else(PoisonError::into_inner);
    f()
}

/// Threads mode: a spawner thread has unshared its file-system attributes,
/// so no spawn takes the fallback from now on (`fallback_spawn` refuses
/// one), and path operations need no lock (module comment, item 4,
/// "Threads mode"). Set once, under `CWD_LOCK` held exclusively, so no
/// fallback spawn is in progress when it becomes true; never cleared.
#[cfg(feature = "threads")]
static NO_FALLBACK: AtomicBool = AtomicBool::new(false);

/// Threads mode: whether a spawn may still take the fallback, so that a
/// path operation must hold `CWD_LOCK` shared. True until the
/// spawner has unshared (or forever where `unshare(CLONE_FS)` is refused),
/// and while the test hook forces the fallback.
#[cfg(feature = "threads")]
fn fallback_possible() -> bool {
    // Acquire: pairs with the Release store, made after the last fallback
    // spawn's way back (under the same exclusive lock), so a caller that
    // sees `true` makes its call after that way back
    !NO_FALLBACK.load(Ordering::Acquire) || fallback_forced()
}

/// Runs `f`, system calls that look up paths (an open, a `stat`, a
/// directory listing, a creation, a removal, a rename, a link, a `chmod`, a
/// `realpath`), so that in threads mode no path resolves against the `cwd`
/// of another thread's fallback spawn (module comment, item 4;
/// docs/threads.md, 3.2; review RT1-04):
/// - in a build with the feature `threads`, while a spawn may take the
///   fallback, `f` holds `CWD_LOCK` shared, as `IO.currentDir` does; an
///   absolute path too, since it can reach the working directory through
///   `/proc/self/cwd` (module comment, item 4);
/// - in a build without it, `f()`: no lock, the same system calls as
///   before.
///
/// `f` makes system calls only: no translator code, no other lock of the
/// crate, and **no call that can wait for another thread or process** (an
/// `open(2)` of a FIFO waits for its other end): a fallback spawn waits for
/// every reader of `CWD_LOCK`, and with std's lock, which prefers a waiting
/// writer, so does every later lookup (review RT2-03). An open of a path the
/// program supplies goes through [`open_looked_up`] instead. A translator's
/// own system calls that resolve paths go through it too. It must not be
/// called with `CWD_LOCK` held (inside a spawn, a change or a read of the
/// working directory): a second read lock of one thread can wait for good
/// behind a waiting writer.
#[inline]
pub fn with_path_lookup<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(feature = "threads")]
    if fallback_possible() {
        return with_cwd_read(f);
    }
    f()
}

/// `open(2)` of a path the program supplies (`Handle.mk`), with `flags` and
/// `mode`. In a build without the feature `threads`, and in threads mode
/// once no spawn takes the fallback, it is that one call. In threads mode
/// while a spawn may take the fallback (module comment, item 4), `CWD_LOCK`
/// is held shared only while the path is looked up, never while the open
/// waits (an open of a FIFO waits for its other end, which a fallback
/// spawn's child may be: review RT2-03):
/// - **a creating open** (`O_CREAT`) is the one open under the lock, with
///   `O_NONBLOCK` added, so it never waits, then `O_NONBLOCK` cleared
///   (`O_APPEND` kept). So every check of a creating open runs, the
///   kernel's `may_create_in_sticky` included (`fs.protected_regular`,
///   `fs.protected_fifos`: another user's file or FIFO in a sticky
///   directory is `EACCES`; review RT2-11), and a new name is created as
///   the one open creates it, through a dangling symbolic link too. Only
///   when that open would wait (`ENXIO`: a FIFO opened for writing with no
///   reader; `EWOULDBLOCK`: a lease to break) is the file opened as below,
///   without `O_CREAT`: the kernel gives those errors in `vfs_open`, after
///   the lookup and after `may_create_in_sticky` (`do_open`), and before any
///   truncation, so the checks have passed and nothing has changed. If the
///   name goes before it is looked up again, the creating open is tried
///   again with `O_NONBLOCK`. `O_NONBLOCK` is not neutral for every
///   character device: a serial port without `CLOCAL` skips the open's
///   carrier wait (later IO waits as natively), and an exclusive device
///   gives `EBUSY` where a blocking open would wait (narrow; not handled);
/// - **any other open** resolves the path under the lock, `open(path,
///   O_PATH | O_CLOEXEC)`, which never opens the file itself, lets go of
///   the lock, and opens the file through the descriptor,
///   `open("/proc/self/fd/N", flags)`: the same file, the same checks of
///   its type and permissions (`EACCES`, `EISDIR`, `ETXTBSY`) and the same
///   waits. Then the `O_PATH` descriptor goes, and the file's descriptor
///   takes the lowest free number, as the one open's would (review RT2-12);
/// - where that cannot work, it is the one open under the lock, as before:
///   without `/proc` (`/proc/self/fd/N` is `ENOENT`), and when the second
///   descriptor is not to be had (`EMFILE`, `ENFILE`: the one open needs
///   one only; RT2-12). Such an open of a FIFO can still wait under the
///   lock.
pub(crate) fn open_looked_up(
    p: &std::path::Path,
    flags: rustix::fs::OFlags,
    mode: rustix::fs::Mode,
) -> rustix::io::Result<OwnedFd> {
    #[cfg(feature = "threads")]
    if fallback_possible() {
        return open_unlocked_wait(p, flags, mode);
    }
    rustix::fs::open(p, flags, mode)
}

/// [`open_looked_up`] while a spawn may take the fallback.
#[cfg(feature = "threads")]
fn open_unlocked_wait(
    p: &std::path::Path,
    flags: rustix::fs::OFlags,
    mode: rustix::fs::Mode,
) -> rustix::io::Result<OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    use rustix::io::Errno;
    let locked_open = || with_cwd_read(|| rustix::fs::open(p, flags, mode));
    let found = loop {
        if flags.contains(OFlags::CREATE) {
            match with_cwd_read(|| rustix::fs::open(p, flags | OFlags::NONBLOCK, mode)) {
                Ok(fd) => {
                    if !flags.contains(OFlags::NONBLOCK) {
                        let now = rustix::fs::fcntl_getfl(&fd)?;
                        rustix::fs::fcntl_setfl(&fd, now - OFlags::NONBLOCK)?;
                    }
                    return Ok(fd);
                }
                // it would wait: opened below, its checks passed
                Err(Errno::NXIO | Errno::WOULDBLOCK) => {}
                Err(e) => return Err(e),
            }
        }
        match with_cwd_read(|| rustix::fs::open(p, OFlags::PATH | OFlags::CLOEXEC, Mode::empty())) {
            Ok(found) => break found,
            // the name went meanwhile: a creating open tries again with
            // `O_NONBLOCK`, not a blocking open under the lock (the name may
            // be a FIFO again by then: review RT2-11, re-check note 3)
            Err(Errno::NOENT) if flags.contains(OFlags::CREATE) => continue,
            Err(e) => return Err(e),
        }
    };
    let fd = format!("/proc/self/fd/{}", found.as_raw_fd());
    let reflags = flags - (OFlags::CREATE | OFlags::EXCL);
    match rustix::fs::open(fd.as_str(), reflags, Mode::empty()) {
        Ok(file) => {
            drop(found);
            Ok(lowest_number(file, flags))
        }
        // no `/proc`, or no second descriptor
        Err(Errno::NOENT | Errno::MFILE | Errno::NFILE) => {
            drop(found);
            locked_open()
        }
        Err(e) => Err(e),
    }
}

/// `fd` at the lowest free descriptor number (`F_DUPFD_CLOEXEC` from 0, the
/// higher one closed), as the one `open(2)` would have numbered it (review
/// RT2-12); `fd` itself when it has the lowest, when it is not close-on-exec
/// (rustix's `F_DUPFD` without the flag is not on Linux; `Handle.mk` always
/// opens close-on-exec), or when the duplication fails.
#[cfg(feature = "threads")]
fn lowest_number(fd: OwnedFd, flags: rustix::fs::OFlags) -> OwnedFd {
    if !flags.contains(rustix::fs::OFlags::CLOEXEC) {
        return fd;
    }
    match rustix::io::fcntl_dupfd_cloexec(&fd, 0) {
        Ok(low) if low.as_raw_fd() < fd.as_raw_fd() => low,
        _ => fd,
    }
}

/// Threads mode (`sched::start`, before any task runs): decide the spawn
/// path now, by starting the spawner thread, so that path operations take
/// no lock from then on where `unshare(CLONE_FS)` works (module comment,
/// item 4, "Threads mode"). A failure to start the thread changes nothing:
/// the first spawn with a `cwd` tries again, and until then path operations
/// take the lock.
#[cfg(feature = "threads")]
pub(crate) fn decide_spawn_path() {
    let _ = spawner();
}

fn fallback_forced() -> bool {
    #[cfg(test)]
    return FORCE_FALLBACK.load(Ordering::Relaxed);
    #[cfg(not(test))]
    false
}

/// Whether this thread could give itself file-system attributes of its own.
fn unshare_fs() -> bool {
    nix::sched::unshare(nix::sched::CloneFlags::CLONE_FS).is_ok()
}

/// The spawner thread's queue, the thread started at the first call; `None`
/// when it could not unshare its file-system attributes (the fallback).
fn spawner() -> Result<Option<mpsc::Sender<Job>>, i32> {
    if fallback_forced() || NO_PRIVATE_CWD.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let mut g = SPAWNER.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(tx) = g.as_ref() {
        return Ok(Some(tx.clone()));
    }
    let (tx, rx) = mpsc::channel::<Job>();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<bool>(1);
    std::thread::Builder::new()
        .name("lean-runtime-spawner".to_owned())
        .spawn(move || {
            let private = unshare_fs();
            let _ = ready_tx.send(private);
            if private {
                for job in rx {
                    job.run();
                }
            }
        })
        .map_err(thread_error)?;
    if ready_rx.recv() == Ok(true) {
        *g = Some(tx.clone());
        // Threads mode: no fallback spawn from now on. Under `CWD_LOCK`
        // held exclusively, so none is in progress (lock order: `SPAWNER`,
        // then `CWD_LOCK`; module comment, item 4).
        #[cfg(feature = "threads")]
        {
            let _w = CWD_LOCK.write().unwrap_or_else(PoisonError::into_inner);
            NO_FALLBACK.store(true, Ordering::Release);
        }
        Ok(Some(tx))
    } else {
        NO_PRIVATE_CWD.store(true, Ordering::Relaxed);
        Ok(None)
    }
}

/// The spawner thread has gone (it cannot, short of a panic): the next spawn
/// starts another.
fn spawner_gone() -> SpawnError {
    *SPAWNER.lock().unwrap_or_else(PoisonError::into_inner) = None;
    SpawnError::Os(EAGAIN)
}

/// A spawn with a `cwd`: on the spawner thread, on a helper thread when the
/// spawner cannot take the caller's nice value, or the fallback.
fn spawn_in(spec: &Arc<Spec>, dups: Vec<(i32, i32)>, cwd: &[u8]) -> Result<Pid, SpawnError> {
    let relative = !cwd.starts_with(b"/");
    match spawner().map_err(SpawnError::Os)? {
        Some(tx) => {
            let (answer, answers) = mpsc::sync_channel(1);
            let job = Job {
                spec: spec.clone(),
                dups,
                base: if relative {
                    // `open(".")`: a lookup (no lock in fact: the spawner
                    // runs, so no fallback spawn happens)
                    with_path_lookup(Base::here).map_err(SpawnError::Os)?
                } else {
                    Base::Unused
                },
                nice: rustix::process::getpriority_process(None).ok(),
                answer,
            };
            if tx.send(job).is_err() {
                return Err(spawner_gone());
            }
            match answers.recv() {
                Ok(Answer::Spawned(r)) => r,
                Ok(Answer::Refused(job)) => on_helper(job),
                Err(_) => Err(spawner_gone()),
            }
        }
        None => fallback_spawn(spec, &dups, cwd, relative),
    }
}

/// The working directory of the last helper thread (`on_helper`) as it
/// ended: the test `helper_leaves_cwd` reads it.
#[cfg(test)]
pub(crate) static HELPER_CWD: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// A job the spawner thread could not run at the caller's nice value: a
/// short-lived thread the caller creates, which has the caller's nice value
/// and signal mask, gives itself file-system attributes of its own and runs
/// it (or, where it cannot, takes the fallback) (review RIO2-14).
fn on_helper(job: Job) -> Result<Pid, SpawnError> {
    let helper = std::thread::Builder::new()
        .name("lean-runtime-spawn".to_owned())
        .spawn(move || {
            let r = if unshare_fs() {
                job.spawn_and_leave()
            } else {
                let cwd = job.spec.cwd.as_deref().unwrap_or_default();
                fallback_spawn(&job.spec, &job.dups, cwd, !cwd.starts_with(b"/"))
            };
            #[cfg(test)]
            {
                *HELPER_CWD.lock().unwrap_or_else(PoisonError::into_inner) =
                    std::fs::read_link("/proc/thread-self/cwd").ok();
            }
            r
        })
        .map_err(|e| SpawnError::Os(thread_error(e)))?;
    helper.join().unwrap_or(Err(SpawnError::Os(EAGAIN)))
}

/// The fallback of module comment item 4: the calling thread enters `cwd`
/// for the process, spawns, and comes back, holding [`CWD_LOCK`]
/// exclusively.
fn fallback_spawn(
    spec: &Spec,
    dups: &[(i32, i32)],
    cwd: &[u8],
    relative: bool,
) -> Result<Pid, SpawnError> {
    let _g = CWD_LOCK.write().unwrap_or_else(PoisonError::into_inner);
    // Threads mode: once a spawner has unshared, other threads' path
    // operations take no lock, so the process must not move; only a
    // later refusal of `unshare` (a helper's, or a new spawner's after the
    // first one ended) gets here then. `EAGAIN`, as a failed `fork`.
    #[cfg(feature = "threads")]
    if !fallback_possible() {
        return Err(SpawnError::Os(EAGAIN));
    }
    // the way back, checked before leaving
    let back = Base::here().map_err(SpawnError::Os)?;
    match back.enter() {
        Ok(()) => {}
        // the child's `chdir` of a relative `cwd` needs the same permission
        Err(_) if relative => return Err(SpawnError::Child(Failure::Cwd)),
        Err(e) => return Err(SpawnError::Os(e.raw_os_error())),
    }
    if rustix::process::chdir(cwd).is_err() {
        return Err(SpawnError::Child(Failure::Cwd));
    }
    let r = spec.spawn_here(dups);
    // Checked above, and no thread of the runtime changes the directory
    // meanwhile; only another process's change to it (its mode, its
    // removal) can make this fail, and the process stays in `cwd` (module
    // comment, item 4).
    let _ = back.enter();
    r
}

// ---- the standard streams ----

/// The child's ends of its standard streams (by target 0, 1, 2) and the
/// parent's ends of its pipes.
struct Ends {
    child: [Option<OwnedFd>; 3],
    parent: [Option<OwnedFd>; 3],
}

impl Ends {
    /// `setup_stdio` for each stream: a pipe for `piped`, `/dev/null`
    /// (read-only for standard input, write-only otherwise) for `null`,
    /// nothing for `inherit`. Every pipe is made first, in stream order, and
    /// `/dev/null` opened after them, so the parent's pipe ends get the same
    /// numbers as natively, where only the forked child opens `/dev/null`.
    /// `/dev/null` is opened here, in the parent, close-on-exec: the program
    /// does not inherit the opened descriptor (LB-15), and a failure to open
    /// it is the spawn's error, not the parent's stream left in place
    /// (LB-17); Lean's forked child opens it without `O_CLOEXEC` and ignores
    /// the failure.
    fn new(cfg: StdioConfig) -> Result<Ends, i32> {
        let mut child = [None, None, None];
        let mut parent = [None, None, None];
        let streams = [cfg.stdin, cfg.stdout, cfg.stderr];
        for (i, s) in streams.into_iter().enumerate() {
            if s == Stdio::Piped {
                let (r, w) = pipe()?;
                let (mine, theirs) = if i == 0 { (w, r) } else { (r, w) };
                child[i] = Some(theirs);
                parent[i] = Some(mine);
            }
        }
        for (i, s) in streams.into_iter().enumerate() {
            if s == Stdio::Null {
                let flags = if i == 0 {
                    rustix::fs::OFlags::RDONLY
                } else {
                    rustix::fs::OFlags::WRONLY
                };
                let f = rustix::fs::open(
                    "/dev/null",
                    flags | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|e| e.raw_os_error())?;
                child[i] = Some(f);
            }
        }
        Ok(Ends { child, parent })
    }

    fn dups(&self) -> Vec<(i32, i32)> {
        self.child
            .iter()
            .enumerate()
            .filter_map(|(i, fd)| fd.as_ref().map(|fd| (fd.as_raw_fd(), i as i32)))
            .collect()
    }
}

/// The parent's end of a child's piped standard input.
struct StdinEnd {
    fd: OwnedFd,
    /// The pipe's reader never reads (a modelled child): the end is
    /// non-blocking, and a full pipe is `EPIPE`
    /// ([`Handle::fdopen_bounded_pipe`]).
    bounded: bool,
}

impl StdinEnd {
    /// The end as `spawn`'s handle (`fdopen` `"w"`).
    fn into_handle(self) -> Handle {
        if self.bounded {
            Handle::fdopen_bounded_pipe(self.fd)
        } else {
            Handle::fdopen(self.fd, FsMode::Write)
        }
    }
}

/// A started child with the parent's ends, as descriptors (`spawn` wraps
/// them in handles, `output` writes and reads them directly).
struct Started {
    stdin: Option<StdinEnd>,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    process: ChildProcess,
}

impl Started {
    /// The parent's ends of a child that runs (the child's close here).
    fn running(ends: Ends, pid: Pid, setsid: bool) -> Started {
        let Ends { child, parent } = ends;
        drop(child);
        let [i, o, e] = parent;
        Started {
            stdin: i.map(|fd| StdinEnd { fd, bounded: false }),
            stdout: o,
            stderr: e,
            process: ChildProcess {
                pid: pid.as_raw_nonzero().get() as u32,
                setsid,
                modelled: None,
            },
        }
    }
}

fn write_all_fd(fd: impl AsFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match rustix::io::write(&fd, bytes) {
            Ok(0) => return,
            Ok(n) => bytes = &bytes[n..],
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

/// Writes `bytes` into a pipe nobody reads yet: at once when they fit
/// (`PIPE_BUF`), else on a thread while the program reads, as the forked
/// child writes them.
fn fill_pipe(w: OwnedFd, bytes: Vec<u8>) -> Result<(), i32> {
    if bytes.len() <= PIPE_BUF {
        write_all_fd(&w, &bytes);
        return Ok(());
    }
    std::thread::Builder::new()
        .name("lean-runtime-child-output".to_owned())
        .spawn(move || write_all_fd(&w, &bytes))
        .map(drop)
        .map_err(thread_error)
}

// ---- a child that cannot start (module comment, item 6) ----

/// How long the stand-in waits after the spawn before it writes and exits:
/// about the life of Lean's forked child that fails (the fork, the child's
/// start, its `execvp` attempts and its message take 1 to 3 ms natively on
/// the reference host). So a write right after the spawn finds its standard
/// input open and one after a delay does not, and the parent's next lines
/// come before the child's message, as natively. A stand-in that exited as
/// soon as `/bin/sh` starts lost the first race to the program most of the
/// time.
const STAND_IN_LIFE: std::time::Duration = std::time::Duration::from_millis(2);

/// The stand-in's descriptor above the standard ones: the release pipe's
/// read end.
const RELEASE_FD: i32 = 3;

/// A descriptor of `fd`'s file numbered above `RELEASE_FD`, so that the
/// stand-in's `dup2`s onto 0 to 3 cannot overwrite it before it is copied.
fn above_targets(fd: OwnedFd) -> Result<OwnedFd, i32> {
    if fd.as_raw_fd() > RELEASE_FD {
        return Ok(fd);
    }
    rustix::io::fcntl_dupfd_cloexec(&fd, RELEASE_FD + 1).map_err(|e| e.raw_os_error())
}

/// Spawns the stand-in, `/bin/sh` running `read x <&3`, then
/// `printf %s "$1"` onto its standard error, then `exit 255`, with the
/// message as `$1`. It gets the child's standard streams `dups`, the read
/// end of `release` (descriptor 3) and a new session when `session`. Once
/// `release`'s write end is closed it writes the message on its standard
/// error, as Lean's forked child does, and exits with 255. It writes nothing
/// on its standard output: the parent's pending standard-output bytes stay
/// the parent's (LB-42).
fn spawn_stand_in(
    dups: &[(i32, i32)],
    release: &OwnedFd,
    message: &CStr,
    session: bool,
) -> Result<Pid, i32> {
    #[cfg(test)]
    if FORCE_NO_SHELL.load(Ordering::Relaxed) {
        return Err(ENOENT);
    }
    let mut actions = PosixSpawnFileActions::init().map_err(|e| e as i32)?;
    for &(fd, target) in dups {
        actions.add_dup2(fd, target).map_err(|e| e as i32)?;
    }
    actions
        .add_dup2(release.as_raw_fd(), RELEASE_FD)
        .map_err(|e| e as i32)?;
    let mut attr = PosixSpawnAttr::init().map_err(|e| e as i32)?;
    if session {
        attr.set_flags(PosixSpawnFlags::from_bits_retain(POSIX_SPAWN_SETSID))
            .map_err(|e| e as i32)?;
    }
    let script: &CStr = c"read x <&3; printf %s \"$1\" >&2; exit 255";
    let argv: [&CStr; 5] = [c"/bin/sh", c"-c", script, c"sh", message];
    let envp: [&CStr; 0] = [];
    posix_spawn(c"/bin/sh", &actions, &attr, &argv, &envp)
        .map_err(|e| e as i32)
        .and_then(to_pid)
}

/// The child that cannot start, as a stand-in process (module comment, item
/// 6): it runs with the child's streams and writes the message itself,
/// `STAND_IN_LIFE` after the spawn, as the forked child writes it about a
/// millisecond later. `None` when it cannot be set up (no `/bin/sh`, no
/// descriptor left): the child is then modelled.
fn stand_in(ends: Ends, spec: &Spec, f: Failure) -> Option<Started> {
    let session = spec.setsid && f == Failure::Program;
    let message = CString::new(spec.failure_message(f)).ok()?;
    let (release, hold) = pipe().ok()?;
    let release = above_targets(release).ok()?;
    // a spawn without a `cwd`: in threads mode never while a fallback
    // spawn has the process in its `cwd` (`with_path_lookup`)
    let pid =
        with_path_lookup(|| spawn_stand_in(&ends.dups(), &release, &message, session)).ok()?;
    drop(release);
    // the stand-in proceeds `STAND_IN_LIFE` after the spawn (at once without
    // a thread)
    let _ = std::thread::Builder::new()
        .name("lean-runtime-stand-in".to_owned())
        .spawn(move || {
            std::thread::sleep(STAND_IN_LIFE);
            drop(hold);
        });
    Some(Started::running(ends, pid, spec.setsid))
}

/// The next pid of a modelled child: above any pid the kernel gives
/// (`pid_max` is at most 2^22), counting down from `0x7FFFFFFF`.
static NEXT_MODELLED_PID: AtomicU32 = AtomicU32::new(0x7FFF_FFFF);

/// One output stream `target` (1 or 2) of a modelled child, which writes
/// `bytes` on it: a `piped` one is a new pipe holding the bytes, then end of
/// file; an `inherit` one is the parent's own descriptor (the forked child's
/// inherited one); a `null` one takes nothing.
fn modelled_output(cfg: Stdio, target: u8, bytes: Vec<u8>) -> Result<Option<OwnedFd>, i32> {
    match cfg {
        Stdio::Piped => {
            let (r, w) = pipe()?;
            fill_pipe(w, bytes)?;
            Ok(Some(r))
        }
        Stdio::Inherit => {
            let fd = if target == 1 {
                rustix::stdio::stdout()
            } else {
                rustix::stdio::stderr()
            };
            write_all_fd(fd, &bytes);
            Ok(None)
        }
        Stdio::Null => Ok(None),
    }
}

/// The child that cannot start, modelled where `/bin/sh` cannot be spawned:
/// a piped standard input whose read end it holds until it is waited,
/// written through a non-blocking end that reports a full pipe as `EPIPE`;
/// a piped standard output at its end at once (nothing of the parent's
/// pending bytes, LB-42); the message on its standard error.
fn modelled_child(cfg: StdioConfig, spec: &Spec, f: Failure) -> Result<Started, i32> {
    let mut stdin_reader = None;
    let stdin = match cfg.stdin {
        Stdio::Piped => {
            let (r, w) = pipe()?;
            rustix::fs::fcntl_setfl(&w, rustix::fs::OFlags::NONBLOCK)
                .map_err(|e| e.raw_os_error())?;
            stdin_reader = Some(r);
            Some(StdinEnd {
                fd: w,
                bounded: true,
            })
        }
        Stdio::Inherit | Stdio::Null => None,
    };
    let stdout = modelled_output(cfg.stdout, 1, Vec::new())?;
    let stderr = modelled_output(cfg.stderr, 2, spec.failure_message(f))?;
    Ok(Started {
        stdin,
        stdout,
        stderr,
        process: ChildProcess {
            pid: NEXT_MODELLED_PID.fetch_sub(1, Ordering::Relaxed),
            setsid: spec.setsid,
            modelled: Some(Arc::new(Mutex::new(Modelled {
                stdin_reader,
                reaped: false,
                at_chdir: f == Failure::Cwd,
            }))),
        },
    })
}

/// `lean_io_process_spawn` up to the parent's ends (see the module comment).
fn start(cfg: StdioConfig, a: &SpawnArgs) -> Result<Started, IoError> {
    // a child process is an effect another context can see
    super::effect_point();
    if cfg.stdin == Stdio::Inherit {
        // `std::cout.flush()`, `fflush(stdout)` under `sync_with_stdio`;
        // its error is not reported
        let _ = Handle::stdout().flush();
    }
    let spec = Arc::new(Spec::new(a));
    let ends = Ends::new(cfg).map_err(os_error)?;
    let launched = match &spec.cwd {
        None => {
            // never while a fallback spawn has the process in its `cwd`
            let _g = CWD_LOCK.read().unwrap_or_else(PoisonError::into_inner);
            spec.spawn_here(&ends.dups())
        }
        Some(cwd) => spawn_in(&spec, ends.dups(), cwd),
    };
    match launched {
        Ok(pid) => Ok(Started::running(ends, pid, spec.setsid)),
        Err(SpawnError::Child(f)) => match stand_in(ends, &spec, f) {
            Some(s) => Ok(s),
            None => modelled_child(cfg, &spec, f).map_err(os_error),
        },
        Err(SpawnError::Os(e)) => Err(os_error(e)),
    }
}

/// `IO.Process.spawn` (`lean_io_process_spawn`).
pub fn spawn(cfg: StdioConfig, args: &SpawnArgs) -> Result<Child, IoError> {
    let s = start(cfg, args)?;
    Ok(Child {
        stdin: s.stdin.map(StdinEnd::into_handle),
        stdout: s.stdout.map(|fd| Handle::fdopen(fd, FsMode::Read)),
        stderr: s.stderr.map(|fd| Handle::fdopen(fd, FsMode::Read)),
        process: s.process,
    })
}

/// `waitpid`'s status as Lean reports it: the exit status, or 128 plus the
/// signal (bash's convention).
fn status_code(st: rustix::process::WaitStatus) -> u32 {
    if let Some(c) = st.exit_status() {
        c as u32
    } else if let Some(s) = st.terminating_signal() {
        128 + s as u32
    } else {
        // `waitpid` without `WUNTRACED` or `WCONTINUED` reports neither
        0
    }
}

impl ChildProcess {
    /// The process object of a child by its pid and `setsid` flag, as Lean's
    /// `Child` object holds them, for a glue that keeps only those two (or
    /// no longer holds the object [`spawn`] returned, after the child is
    /// reaped): [`wait`](Self::wait), [`try_wait`](Self::try_wait) and
    /// [`kill`](Self::kill) then make the system call on the pid, as
    /// `lean_io_process_child_wait` & co. do.
    ///
    /// What the crate assumes about `pid`:
    /// - It is the pid of a child this process spawned, or of one that is
    ///   gone. The calls on it are those of native's object: `waitpid(pid)`
    ///   gives the child's status once, then `ECHILD`; `kill` signals the
    ///   pid (its group with `setsid`), `ESRCH` when no process has that id
    ///   any more (unless the system gave it to another process meanwhile,
    ///   natively too).
    /// - It is positive. Lean's `Child.pid` always is. For 0, or above
    ///   `i32::MAX`, the calls fail as on a pid that is gone (`ECHILD`,
    ///   `ESRCH`), where natively `waitpid(0)` and `kill(0)` would reach a
    ///   process group.
    /// - It is not a modelled child's (module comment, item 6: a child that
    ///   could not start when no `/bin/sh` could stand in): such a child has
    ///   no process, so only the object [`spawn`] returned (or its clones)
    ///   can wait for it. Built from its pid, it is a child that is gone.
    pub fn from_pid(pid: u32, setsid: bool) -> ChildProcess {
        ChildProcess {
            pid,
            setsid,
            modelled: None,
        }
    }

    /// `Child.pid` (`lean_io_process_child_pid`).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The `setsid` flag of Lean's child object.
    pub fn setsid(&self) -> bool {
        self.setsid
    }

    /// The process of the child `Child.takeStdin` returns
    /// (`lean_io_process_child_take_stdin`): the same process, with its
    /// `setsid` flag, so `kill` still reaches the group. Lean's new object
    /// holds the pid only and its flag reads zeroed padding (LB-14). The
    /// translator moves the standard-input field out and gives the new child
    /// `()` there.
    pub fn take_stdin(&self) -> ChildProcess {
        self.clone()
    }

    fn modelled(&self) -> Option<std::sync::MutexGuard<'_, Modelled>> {
        self.modelled
            .as_ref()
            .map(|f| f.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// `Child.wait` (`lean_io_process_child_wait`, `waitpid`): the exit code.
    pub fn wait(&self) -> Result<u32, IoError> {
        if let Some(mut f) = self.modelled() {
            return f.reap().map_err(os_error);
        }
        waitpid_once(self.pid, WaitOptions::empty()).map(|st| st.map_or(0, status_code))
    }

    /// `Child.tryWait` (`lean_io_process_child_try_wait`, `waitpid` with
    /// `WNOHANG`): `None` while the child runs.
    pub fn try_wait(&self) -> Result<Option<u32>, IoError> {
        if let Some(mut f) = self.modelled() {
            return f.reap().map(Some).map_err(os_error);
        }
        waitpid_once(self.pid, WaitOptions::NOHANG).map(|st| st.map(status_code))
    }

    /// `Child.kill` (`lean_io_process_child_kill`): `SIGKILL` to the child,
    /// or to its process group (`killpg`) with `setsid`.
    pub fn kill(&self) -> Result<(), IoError> {
        super::effect_point();
        if let Some(f) = self.modelled() {
            // a zombie until waited; a child that failed at `chdir` never
            // called `setsid()`, so no group has its id
            return if f.reaped || (self.setsid && f.at_chdir) {
                Err(os_error(ESRCH))
            } else {
                Ok(())
            };
        }
        let pid = child_pid(self.pid).ok_or_else(|| os_error(ESRCH))?;
        let r = if self.setsid {
            rustix::process::kill_process_group(pid, Signal::KILL)
        } else {
            rustix::process::kill_process(pid, Signal::KILL)
        };
        r.map_err(|e| os_error(e.raw_os_error()))
    }
}

impl Modelled {
    /// The forked child exited at once with status 255: the first wait reaps
    /// it and closes its standard input; later ones find no child.
    fn reap(&mut self) -> Result<u32, i32> {
        if self.reaped {
            return Err(ECHILD);
        }
        self.reaped = true;
        self.stdin_reader = None;
        Ok(255)
    }
}

/// A child's pid as rustix's `Pid`: `None` for 0 and for a value above
/// `i32::MAX` (negative as a `pid_t`). To `waitpid` and `kill` those name a
/// process group, any child or every process (`kill(-1, SIGKILL)`), never
/// one child, so a pid of [`ChildProcess::from_pid`] that is not positive
/// is taken as a child that is gone.
fn child_pid(pid: u32) -> Option<Pid> {
    i32::try_from(pid).ok().and_then(Pid::from_raw)
}

/// `waitpid(pid, &status, options)` once (Lean does not retry `EINTR`). A
/// blocking wait in a program with tasks first lets the other contexts run
/// until the child has exited (sched-io, `io::coop`).
fn waitpid_once(
    pid: u32,
    opts: WaitOptions,
) -> Result<Option<rustix::process::WaitStatus>, IoError> {
    let pid = child_pid(pid).ok_or_else(|| os_error(ECHILD))?;
    #[cfg(feature = "sched")]
    if opts.is_empty() && crate::sched::coop_possible() {
        super::coop::before_waitpid(pid);
    }
    match rustix::process::waitpid(Some(pid), opts) {
        Ok(r) => Ok(r.map(|(_, st)| st)),
        Err(e) => Err(os_error(e.raw_os_error())),
    }
}

/// A UTF-8 check over a byte stream that arrives in pieces (`readToEnd`'s
/// `String.fromUTF8?`, which is standard UTF-8, as Rust's).
#[derive(Default)]
struct Utf8Stream {
    carry: [u8; 4],
    n: usize,
    bad: bool,
}

impl Utf8Stream {
    fn feed(&mut self, mut chunk: &[u8]) {
        if self.bad {
            return;
        }
        while self.n > 0 {
            let Some((&b, rest)) = chunk.split_first() else {
                return;
            };
            chunk = rest;
            self.carry[self.n] = b;
            self.n += 1;
            match std::str::from_utf8(&self.carry[..self.n]) {
                Ok(_) => self.n = 0,
                Err(e) if e.error_len().is_none() && self.n < 4 => {}
                Err(_) => {
                    self.bad = true;
                    return;
                }
            }
        }
        if let Err(e) = std::str::from_utf8(chunk) {
            match e.error_len() {
                Some(_) => self.bad = true,
                None => {
                    let rest = &chunk[e.valid_up_to()..];
                    self.carry[..rest.len()].copy_from_slice(rest);
                    self.n = rest.len();
                }
            }
        }
    }

    fn valid(&self) -> bool {
        !self.bad && self.n == 0
    }
}

/// `Handle.readToEnd`'s error for bytes that are not UTF-8.
fn not_utf8() -> IoError {
    IoError::user_error("Tried to read from handle containing non UTF-8 data.")
}

/// One pipe of `output` being read to its end.
struct Reading<'a, S: ByteSink + ?Sized> {
    fd: Option<OwnedFd>,
    sink: &'a mut S,
    utf8: Utf8Stream,
    error: Option<i32>,
}

impl<S: ByteSink + ?Sized> Reading<'_, S> {
    /// Whether the sink has stopped taking bytes ([`ByteSink::stopped`]).
    #[inline]
    fn stopped(&self) -> bool {
        self.sink.stopped()
    }

    /// One `read` into `buf`'s spare capacity (no zeroing), appended to the
    /// sink; the descriptor is dropped at end of file or on an error.
    fn step(&mut self, buf: &mut Vec<u8>) {
        let Some(fd) = &self.fd else { return };
        buf.clear();
        match rustix::io::read(fd, buf.spare_capacity_mut()) {
            Ok(([], _)) => self.fd = None,
            Ok((got, _)) => {
                self.utf8.feed(got);
                self.sink.extend_from_slice(got);
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => {
                self.error = Some(e.raw_os_error());
                self.fd = None;
            }
        }
    }
}

/// The drains `output` left running ([`drain_in_background`]), joined once
/// `main` has returned ([`join_drains`]).
static DRAINS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Reads a pipe to its end on a thread of its own: `output`'s standard-output
/// task, a dedicated task in Lean, still running when `output` has failed.
/// The process waits for it after `main` ([`join_drains`]), as
/// `lean_finalize_task_manager` waits for the dedicated tasks, so the
/// child's later writes still find a reader (AR-6). Without a thread, the
/// pipe closes here (a child still writing then gets `EPIPE`).
///
/// It keeps what it reads, as native's task grows the `ByteArray` that
/// `readToEnd` returns into a result nobody reads: the storage grows by
/// fallible reservations, and when one fails the thread ends the process
/// with Lean's `INTERNAL PANIC: out of memory`, as native's task does from
/// its own thread (review RFX1-04; case `process/output_drain_oom`). So a
/// child that writes without end ends the program, natively and here, where
/// a drain that dropped the bytes made the exit's join wait for good.
fn drain_in_background(fd: Option<OwnedFd>) {
    if let Some(fd) = fd {
        let spawned = std::thread::Builder::new()
            .name("lean-runtime-output-drain".to_owned())
            .spawn(move || {
                let mut kept: Vec<u8> = Vec::new();
                let mut buf = Vec::with_capacity(READ_CHUNK);
                loop {
                    buf.clear();
                    match rustix::io::read(&fd, buf.spare_capacity_mut()) {
                        Ok(([], _)) => return,
                        Ok((got, _)) => {
                            if kept.try_reserve(got.len()).is_err() {
                                drain_out_of_memory();
                            }
                            kept.extend_from_slice(got);
                        }
                        Err(rustix::io::Errno::INTR) => {}
                        Err(_) => return,
                    }
                }
            });
        if let Ok(h) = spawned {
            let mut drains = DRAINS.lock().unwrap_or_else(PoisonError::into_inner);
            // a finished drain's handle is let go (its thread has ended)
            drains.retain(|d| !d.is_finished());
            drains.push(h);
        }
    }
}

/// The drain's storage could not grow: Lean's `lean_internal_panic_out_of_memory`
/// from that thread ([`super::panic::internal_panic`]: its line on standard
/// error, built on the stack, then `exit(1)`, or an abort under
/// `LEAN_ABORT_ON_PANIC`).
fn drain_out_of_memory() -> ! {
    use crate::semantics::panic::InternalPanic;
    super::panic::internal_panic(
        InternalPanic::OutOfMemory.message(),
        &mut super::panic::Native,
    )
}

/// Waits until every drain `output` left running has read its pipe to the
/// end: the dedicated tasks that `lean_finalize_task_manager` waits for
/// after `main` returns (`~task_manager`). A child that never closes its
/// standard output keeps the process alive, as natively. Called by
/// [`super::exit::after_main`].
pub(crate) fn join_drains() {
    loop {
        let drains = std::mem::take(&mut *DRAINS.lock().unwrap_or_else(PoisonError::into_inner));
        if drains.is_empty() {
            return;
        }
        for d in drains {
            let _ = d.join();
        }
    }
}

/// What `output` returns once a sink has stopped ([`ByteSink::stopped`]):
/// `ENOMEM`'s error (`resource exhausted`, errno untouched). The glue, whose
/// sink stopped, ends the process with Lean's `INTERNAL PANIC: out of memory`
/// instead of using it.
fn sink_stopped() -> IoError {
    IoError::decode_io_error(ENOMEM, None)
}

/// Before `output` blocks in `poll` or a read of its pipes: in a program
/// with tasks, wait until one of `reads` is readable or `write` (the input's
/// pipe) is writable, letting the other contexts run (sched-io, `io::coop`).
#[inline]
fn ready(reads: &[&Option<OwnedFd>], write: Option<&OwnedFd>) {
    #[cfg(feature = "sched")]
    if crate::sched::coop_possible() && crate::sched::io_cooperative() {
        let b: Vec<_> = reads
            .iter()
            .filter_map(|f| f.as_ref().map(|f| f.as_fd()))
            .collect();
        super::coop::before_ready_any(&b, write.map(|w| w.as_fd()));
    }
    #[cfg(not(feature = "sched"))]
    let _ = (reads, write);
}

/// Waits until one of `output`'s open ends is ready, and says which:
/// standard output `o` or error `e` readable (data, end of file or an
/// error), the input's pipe `w` writable (room, or the child's end closed).
/// `EINTR` reports none.
fn poll_ends(
    o: &Option<OwnedFd>,
    e: &Option<OwnedFd>,
    w: Option<&OwnedFd>,
) -> Result<[bool; 3], IoError> {
    use nix::poll::{PollFd, PollFlags};
    ready(&[o, e], w);
    let ends = [
        (o.as_ref(), PollFlags::POLLIN),
        (e.as_ref(), PollFlags::POLLIN),
        (w, PollFlags::POLLOUT),
    ];
    let mut fds = Vec::with_capacity(3);
    let mut at = [None; 3];
    for (i, (fd, flags)) in ends.into_iter().enumerate() {
        if let Some(fd) = fd {
            at[i] = Some(fds.len());
            fds.push(PollFd::new(fd.as_fd(), flags));
        }
    }
    match nix::poll::poll(&mut fds, nix::poll::PollTimeout::NONE) {
        Ok(_) => Ok(at.map(|k| k.is_some_and(|k| fds[k].any().unwrap_or(true)))),
        Err(Errno::EINTR) => Ok([false; 3]),
        Err(x) => Err(os_error(x as i32)),
    }
}

/// `output`'s input being written into the child's standard input while
/// its pipes are read (LB-40).
struct Writing<'a> {
    fd: OwnedFd,
    rest: &'a [u8],
    /// The pipe's reader never reads (a modelled child, [`StdinEnd`]).
    bounded: bool,
}

impl<'a> Writing<'a> {
    /// The input's writer over the parent's end of the pipe, which is made
    /// non-blocking (it is an open file description of its own: the child's
    /// end stays blocking). `None` when nothing is left to write, the end
    /// then closed (the child's end of file): an empty input, or the input
    /// of a modelled child, which is written here at once (its pipe is never
    /// read, so a write into the full pipe is `EPIPE`, never a wait). A write
    /// error is `Err`.
    fn start(end: StdinEnd, bytes: &'a [u8]) -> Result<Option<Writing<'a>>, IoError> {
        if bytes.is_empty() {
            return Ok(None);
        }
        if !end.bounded {
            rustix::fs::fcntl_setfl(&end.fd, rustix::fs::OFlags::NONBLOCK)
                .map_err(|e| os_error(e.raw_os_error()))?;
        }
        let mut w = Writing {
            fd: end.fd,
            rest: bytes,
            bounded: end.bounded,
        };
        if w.bounded {
            while !w.step().map_err(os_error)? {}
            return Ok(None);
        }
        Ok(Some(w))
    }

    /// One `write(2)` of what the pipe takes now: `Ok(true)` once every byte
    /// is written (the caller then drops the writer, which closes the pipe),
    /// `Ok(false)` while bytes remain, or the write's `errno` (`EPIPE` once
    /// the child has closed its standard input; a bounded pipe that is full).
    fn step(&mut self) -> Result<bool, i32> {
        match rustix::io::write(&self.fd, self.rest) {
            Ok(n) => {
                self.rest = &self.rest[n..];
                Ok(self.rest.is_empty())
            }
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(rustix::io::Errno::AGAIN) if self.bounded => Err(EPIPE),
            Err(rustix::io::Errno::AGAIN) => Ok(false),
            Err(e) => Err(e.raw_os_error()),
        }
    }
}

/// `IO.Process.output` (Lean code in `Init/System/IO.lean`, here an
/// override): `spawn` with standard output and error piped and standard
/// input `null`, or `piped` when `input` is given. Both pipes are read to
/// their end into `out` and `err`, the caller's storage, on the calling
/// thread with `poll` (Lean reads standard output on a dedicated task while
/// it reads standard error, so neither pipe can block the child).
///
/// **The input** is written into the child's standard input while both
/// pipes are read: `poll` waits for room in its pipe too, and the pipe
/// closes once every byte is in (the child's end of file). Lean writes all
/// of it (`putStr`, `flush`) before it reads anything, so a child that
/// fills its standard output's pipe before it has read all its input (`cat`
/// with more input than a pipe holds) waits for good, and so does Lean's
/// `output` (LB-40, `docs/lean-bugs.md`; case `process/output_large_input`).
/// A write error (`EPIPE` once the child has closed its standard input; a
/// modelled child's full pipe) ends `output` at once, before any error of
/// the reads, as Lean's `putStr` error comes first: the child is not waited,
/// and the pipes close as `output` returns.
///
/// Then, in Lean's order: a read error of standard error, or standard error
/// that is not UTF-8
/// (`Tried to read from handle containing non UTF-8 data.`), fails before
/// the child is waited (its standard output is still read to its end on a
/// thread, which keeps the bytes as Lean's task does, until `main` has
/// returned: [`drain_in_background`]); then `wait`; then standard output's
/// read error or
/// UTF-8 error. Returns the exit code; the sinks then hold valid UTF-8.
///
/// **A sink that stops** ([`ByteSink::stopped`], its storage could not grow;
/// AR-5; the crate's [`super::StoppingSink`]): `output` returns at once, without reading either pipe further and
/// without waiting for the child, and the glue ends the process with Lean's
/// `INTERNAL PANIC: out of memory`, exit status 1. Natively the allocation
/// of the growing `ByteArray` fails in `readToEnd` (the standard-output task
/// or `main`'s standard-error read) and `lean_internal_panic` calls `exit(1)`
/// there: the child is neither waited nor killed, and its pipes close with
/// the process, so its next write gets `EPIPE` (`SIGPIPE` is ignored, as in
/// the parent). Here both pipes close as `output` returns (the parent's
/// ends were its only ones), the child is not waited either, and the result
/// is `ENOMEM`'s error, which the glue, whose sink said it stopped, does not
/// use. Case `process/output_oom` (both pipes, under `ulimit -v`). Native
/// hangs instead when the child keeps the other pipe open without writing
/// to it (`yes` alone): `exit`'s flush of every `FILE` (`_IO_flush_all`)
/// waits for the lock of the other pipe's stream, which the other thread
/// holds in its blocked `fread` (LB-29, not reproduced: this crate reads the
/// pipes without a `FILE`, and its exit waits for no held stream; case
/// `process/output_oom_both_pipes`).
pub fn output<O, E>(
    args: &SpawnArgs,
    input: Option<&[u8]>,
    out: &mut O,
    err: &mut E,
) -> Result<u32, IoError>
where
    O: ByteSink + ?Sized,
    E: ByteSink + ?Sized,
{
    let cfg = StdioConfig {
        stdin: if input.is_some() {
            Stdio::Piped
        } else {
            Stdio::Null
        },
        stdout: Stdio::Piped,
        stderr: Stdio::Piped,
    };
    let s = start(cfg, args)?;
    let mut w = match (s.stdin, input) {
        (Some(end), Some(bytes)) => Writing::start(end, bytes)?,
        _ => None,
    };
    let mut o = Reading {
        fd: s.stdout,
        sink: out,
        utf8: Utf8Stream::default(),
        error: None,
    };
    let mut e = Reading {
        fd: s.stderr,
        sink: err,
        utf8: Utf8Stream::default(),
        error: None,
    };
    let mut buf = Vec::with_capacity(READ_CHUNK);
    // standard error to its end and the input written, standard output read
    // meanwhile
    while e.fd.is_some() || w.is_some() {
        if o.fd.is_none() && w.is_none() {
            ready(&[&e.fd], None);
            e.step(&mut buf);
            if e.stopped() {
                return Err(sink_stopped());
            }
            continue;
        }
        let [ro, re, rw] = poll_ends(&o.fd, &e.fd, w.as_ref().map(|w| &w.fd))?;
        if rw {
            if let Some(x) = &mut w {
                match x.step() {
                    // the input's pipe closes: the child's end of file
                    Ok(true) => w = None,
                    Ok(false) => {}
                    Err(code) => return Err(os_error(code)),
                }
            }
        }
        if ro {
            o.step(&mut buf);
        }
        if re {
            e.step(&mut buf);
        }
        if o.stopped() || e.stopped() {
            return Err(sink_stopped());
        }
    }
    if let Some(x) = e.error {
        drain_in_background(o.fd.take());
        return Err(os_error(x));
    }
    if !e.utf8.valid() {
        drain_in_background(o.fd.take());
        return Err(not_utf8());
    }
    while o.fd.is_some() {
        ready(&[&o.fd], None);
        o.step(&mut buf);
        if o.stopped() {
            return Err(sink_stopped());
        }
    }
    let code = s.process.wait()?;
    if let Some(x) = o.error {
        return Err(os_error(x));
    }
    if !o.utf8.valid() {
        return Err(not_utf8());
    }
    Ok(code)
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
