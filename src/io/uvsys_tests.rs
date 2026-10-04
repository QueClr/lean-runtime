//! Tests of `uvsys.rs`, ported from leanrs's `rt/leanrs_rt/tests/io.rs`
//! (`io_os_*`, `io_uv_*`, `io_uptime`, `io_cpu_info`, the memory queries).
//! Expected values are native Lean's (leanrs probe `validate/io/uv_system` on
//! v4.34.0-rc1, rechecked on 4.34.0 by `tests/cases/uvsys`). A test that
//! changes the process's working directory, environment, priority or title
//! runs in a child process of its own. Miri cannot run these system calls.
#![cfg(not(miri))]

use super::*;
use std::process::Command;

const CHILD: &str = "LEAN_RUNTIME_TEST_CHILD";

fn child_case() -> Option<String> {
    std::env::var(CHILD).ok()
}

/// This test binary again, running only `test` with `CHILD=case`.
fn child(test: &str, case: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        &format!("io::uvsys::tests::{test}"),
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, case);
    c
}

#[track_caller]
fn ok(mut c: Command) {
    let out = c.output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

fn text<E: std::fmt::Debug>(f: impl FnOnce(&mut Vec<u8>) -> Result<(), E>) -> String {
    let mut v = Vec::new();
    f(&mut v).unwrap();
    String::from_utf8(v).unwrap()
}

fn einval() -> IoError {
    IoError::InvalidArgument(None, 22, "invalid argument".to_owned())
}

fn nul(s: &str) -> IoError {
    IoError::InvalidArgument(
        Some(s.to_owned()),
        22,
        "string contains NUL bytes".to_owned(),
    )
}

fn esrch() -> IoError {
    IoError::NoSuchThing(None, 3, "no such process".to_owned())
}

fn enobufs() -> IoError {
    IoError::ResourceExhausted(None, 105, "no buffer space available".to_owned())
}

#[test]
fn io_os_getpid() {
    assert_eq!(os_getpid(), u64::from(std::process::id()));
    assert_eq!(os_getppid(), u64::from(std::os::unix::process::parent_id()));
}

/// `cwd` (`uv_cwd`): `getcwd`'s path.
#[test]
fn io_uv_cwd() {
    let here = std::env::current_dir().unwrap();
    assert_eq!(text(cwd), here.to_str().unwrap());
}

/// `chdir` (`uv_chdir`); lean (uv_system): a missing directory is
/// `no such file or directory (error code: 2)\n  file: no-such-dir`, a NUL
/// byte Lean's embedded-NUL error, a file `ENOTDIR` naming it.
#[test]
fn io_uv_chdir() {
    if child_case().as_deref() == Some("chdir") {
        let tmp = std::env::temp_dir();
        chdir(tmp.to_str().unwrap().as_bytes()).unwrap();
        assert_eq!(
            std::env::current_dir().unwrap(),
            tmp.canonicalize().unwrap()
        );
        return;
    }
    assert_eq!(
        chdir(b"no-such-dir").unwrap_err(),
        IoError::NoFileOrDirectory(
            "no-such-dir".to_owned(),
            2,
            "no such file or directory".to_owned()
        )
    );
    assert_eq!(chdir(b"a\0b").unwrap_err(), nul("a\0b"));
    assert_eq!(
        chdir(b"/proc/self/stat").unwrap_err(),
        IoError::InappropriateType(
            Some("/proc/self/stat".to_owned()),
            20,
            "not a directory".to_owned()
        )
    );
    ok(child("io_uv_chdir", "chdir"));
}

/// `exePath` (`uv_exepath`): `/proc/self/exe`'s target.
#[test]
fn io_uv_exepath() {
    let exe = std::fs::read_link("/proc/self/exe").unwrap();
    assert_eq!(text(exepath), exe.to_str().unwrap());
}

/// `osTmpdir` (`uv_os_tmpdir`); lean (uv_system, RtUvSysLimits): `TMPDIR=/tmp/x/`
/// gives `/tmp/x`, unset `/tmp`, empty the empty string, `TMP` after an unset
/// `TMPDIR`, 4096 bytes `ENOBUFS`.
#[test]
fn io_uv_os_tmpdir() {
    let long = format!("/{}", "t".repeat(4095));
    let at_limit = format!("/{}", "t".repeat(4094));
    if let Some(case) = child_case() {
        let got = {
            let mut v = Vec::new();
            os_tmpdir(&mut v).map(|()| String::from_utf8(v).unwrap())
        };
        match case.as_str() {
            "slash" => assert_eq!(got.unwrap(), "/tmp/x"),
            "unset" => assert_eq!(got.unwrap(), "/tmp"),
            "empty" => assert_eq!(got.unwrap(), ""),
            "tmp" => assert_eq!(got.unwrap(), "/fromTMP"),
            "root" => assert_eq!(got.unwrap(), "/"),
            "long" => assert_eq!(got.unwrap_err(), enobufs()),
            "at-limit" => assert_eq!(got.unwrap(), at_limit),
            _ => unreachable!(),
        }
        return;
    }
    for (case, tmpdir, tmp) in [
        ("slash", Some("/tmp/x/"), None),
        ("unset", None, None),
        ("empty", Some(""), Some("/fromTMP")),
        ("tmp", None, Some("/fromTMP")),
        ("root", Some("/"), None),
        ("long", Some(long.as_str()), None),
        ("at-limit", Some(at_limit.as_str()), None),
    ] {
        let mut c = child("io_uv_os_tmpdir", case);
        c.env_remove("TEMP")
            .env_remove("TEMPDIR")
            .env_remove("TMP")
            .env_remove("TMPDIR");
        if let Some(v) = tmpdir {
            c.env("TMPDIR", v);
        }
        if let Some(v) = tmp {
            c.env("TMP", v);
        }
        ok(c);
    }
}

/// `osEnviron` (`uv_os_environ`): `environ`'s entries in order, an entry
/// whose only `=` is its first byte (`=x`, which `std`'s `vars_os` skips) the
/// empty name, `=a=b` the empty name with value `a=b`, an entry without `=`
/// skipped; after `osSetenv` of an existing name in place, `osUnsetenv`, and
/// `osSetenv` of a new name at the end.
#[test]
fn io_os_environ() {
    if child_case().as_deref() == Some("environ") {
        let shown = || {
            let mut v = Vec::new();
            os_environ(|k, val| {
                if k != CHILD.as_bytes() {
                    v.push(format!(
                        "{}={}",
                        String::from_utf8_lossy(k),
                        String::from_utf8_lossy(val)
                    ));
                }
            });
            v
        };
        assert_eq!(shown(), ["=x", "=a=b", "A=1", "B=two=2"]);
        os_setenv(b"A", b"9").unwrap();
        os_unsetenv(b"B").unwrap();
        os_setenv(b"C", b"3").unwrap();
        assert_eq!(shown(), ["=x", "=a=b", "A=9", "C=3"]);
        assert_eq!(std::env::var("A").unwrap(), "9");
        assert!(std::env::var_os("B").is_none());
        return;
    }
    // `Command` sorts the child's environment by name: `=x`, `=a=b`, `A=1`, `B=two=2`, CHILD
    let mut c = child("io_os_environ", "environ");
    c.env_clear()
        .env("", "x")
        .env("=a", "b")
        .env("A", "1")
        .env("B", "two=2")
        .env(CHILD, "environ");
    ok(c);
}

/// `osGetenv` (`uv_os_getenv`): a name holding a NUL byte, empty or unset is
/// `none`.
#[test]
fn io_os_getenv() {
    let mut v = Vec::new();
    assert!(!os_getenv(b"a\0b", &mut v));
    assert!(!os_getenv(b"LEAN_RUNTIME_NO_SUCH_VARIABLE_XYZ", &mut v));
    assert!(!os_getenv(b"", &mut v));
    if let Some(path) = std::env::var_os("PATH") {
        assert!(os_getenv(b"PATH", &mut v));
        assert_eq!(v, path.as_bytes());
    }
}

/// `osSetenv` and `osUnsetenv`; lean (uv_system): `A=B` and the empty name
/// are `invalid argument (error code: 22, invalid argument)`, a NUL byte the
/// embedded-NUL error naming the string. The changes run in a child.
#[test]
fn io_os_setenv() {
    if child_case().as_deref() == Some("setenv") {
        os_setenv(b"LEAN_RUNTIME_UV_X", b"set by uv").unwrap();
        assert_eq!(std::env::var("LEAN_RUNTIME_UV_X").unwrap(), "set by uv");
        let mut v = Vec::new();
        assert!(os_getenv(b"LEAN_RUNTIME_UV_X", &mut v) && v == b"set by uv");
        os_unsetenv(b"LEAN_RUNTIME_UV_X").unwrap();
        assert!(std::env::var_os("LEAN_RUNTIME_UV_X").is_none());
        os_unsetenv(b"LEAN_RUNTIME_NEVER_SET").unwrap();
        return;
    }
    assert_eq!(os_setenv(b"A=B", b"x").unwrap_err(), einval());
    assert_eq!(os_setenv(b"", b"x").unwrap_err(), einval());
    assert_eq!(os_setenv(b"A\0", b"x").unwrap_err(), nul("A\0"));
    assert_eq!(os_setenv(b"A", b"x\0y").unwrap_err(), nul("x\0y"));
    assert_eq!(os_unsetenv(b"A=B").unwrap_err(), einval());
    assert_eq!(os_unsetenv(b"").unwrap_err(), einval());
    assert_eq!(os_unsetenv(b"A\0").unwrap_err(), nul("A\0"));
    ok(child("io_os_setenv", "setenv"));
}

/// `uptime` (`uv_uptime`): `/proc/uptime`'s first number, cut.
#[test]
fn io_uptime() {
    let t = std::fs::read_to_string("/proc/uptime").unwrap();
    let first: f64 = t.split_whitespace().next().unwrap().parse().unwrap();
    let up = uptime().unwrap();
    assert!(up >= first as u64 && up <= first as u64 + 2, "{up} {first}");
    assert_eq!(scan_f64(b"  12345.67 2.5\n"), Some(12345.67));
    assert_eq!(scan_f64(b"x"), None);
}

/// `getProcessTitle` and `setProcessTitle`: `argv[0]`, then a title cut to
/// the arguments' memory less one byte, the thread's name its first 15
/// bytes; 512 bytes or more is `ENOBUFS` (native case `process_title`). The
/// title is the process's: a child.
#[test]
fn io_process_title() {
    let a0 = std::env::args_os().next().unwrap();
    if child_case().as_deref() == Some("title") {
        let cap: usize = std::env::args_os().map(|a| a.len() + 1).sum();
        assert_eq!(text(get_process_title).as_bytes(), a0.as_bytes());
        set_process_title(b"short").unwrap();
        assert_eq!(text(get_process_title), "short");
        assert_eq!(
            std::fs::read_to_string("/proc/thread-self/comm").unwrap(),
            "short\n"
        );
        let long = vec![b'T'; cap + 10];
        set_process_title(&long).unwrap();
        let mut v = Vec::new();
        let r = get_process_title(&mut v);
        if cap > 512 {
            assert_eq!(r.unwrap_err(), enobufs());
        } else {
            assert_eq!(v.len(), cap - 1);
        }
        assert_eq!(
            std::fs::read_to_string("/proc/thread-self/comm").unwrap(),
            "TTTTTTTTTTTTTTT\n"
        );
        assert_eq!(set_process_title(b"a\0b").unwrap_err(), nul("a\0b"));
        set_process_title(b"").unwrap();
        assert_eq!(text(get_process_title), "");
        return;
    }
    ok(child("io_process_title", "title"));
}

/// `osGetPasswd` (`uv_os_get_passwd`): the effective user's entry.
#[test]
fn io_os_get_passwd() {
    let p = os_get_passwd().unwrap();
    let uid = nix::unistd::Uid::effective().as_raw();
    assert_eq!(p.uid, Some(u64::from(uid)));
    assert!(!p.username.is_empty());
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    if let Some(line) = passwd
        .lines()
        .find(|l| l.split(':').nth(2) == Some(&uid.to_string()))
    {
        let f: Vec<&str> = line.split(':').collect();
        assert_eq!(p.username, f[0].as_bytes());
        assert_eq!(p.homedir.as_deref(), Some(f[5].as_bytes()));
        assert_eq!(p.shell.as_deref(), Some(f[6].as_bytes()));
    }
}

/// `osGetGroup` (`uv_os_get_group`): the user's group; none for a missing
/// one; the gid cut to 32 bits (RtSystem).
#[test]
fn io_os_get_group() {
    let gid = nix::unistd::Gid::effective().as_raw();
    let g = os_get_group(u64::from(gid)).unwrap().unwrap();
    assert_eq!(g.gid, u64::from(gid));
    assert!(!g.groupname.is_empty());
    assert_eq!(os_get_group(u64::from(gid) + (1 << 32)).unwrap(), Some(g));
    assert_eq!(os_get_group(4_000_000).unwrap(), None);
}

/// `osHomedir` (`uv_os_homedir`): `HOME` when set, even empty, else the
/// user's home directory; 4096 bytes `ENOBUFS`.
#[test]
fn io_os_homedir() {
    let long = format!("/{}", "h".repeat(4095));
    match child_case().as_deref() {
        Some("home") => return assert_eq!(text(os_homedir), "/some/home"),
        Some("empty") => return assert_eq!(text(os_homedir), ""),
        Some("unset") => {
            let p = os_get_passwd().unwrap();
            return assert_eq!(Some(text(os_homedir).into_bytes()), p.homedir);
        }
        Some("long") => return assert_eq!(os_homedir(&mut Vec::new()).unwrap_err(), enobufs()),
        _ => {}
    }
    for (case, value) in [
        ("home", Some("/some/home")),
        ("empty", Some("")),
        ("unset", None),
        ("long", Some(long.as_str())),
    ] {
        let mut c = child("io_os_homedir", case);
        match value {
            Some(v) => c.env("HOME", v),
            None => c.env_remove("HOME"),
        };
        ok(c);
    }
}

/// `osGetHostname` (`gethostname`): the kernel's host name.
#[test]
fn io_os_gethostname() {
    let h = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
    assert_eq!(text(os_gethostname), h.trim_end_matches('\n'));
}

/// `osUname` (`uname`); lean (uv_system): `Linux`.
#[test]
fn io_os_uname() {
    let u = os_uname().unwrap();
    assert_eq!(u.sysname, b"Linux");
    assert_eq!(u.machine, std::env::consts::ARCH.as_bytes());
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap();
    assert_eq!(u.release, release.trim_end_matches('\n').as_bytes());
}

/// `hrtime` (`uv_hrtime`): `CLOCK_MONOTONIC` in nanoseconds.
#[test]
fn io_hrtime() {
    let a = hrtime();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b = hrtime();
    assert!(a > 0 && b >= a + 2_000_000, "{a} {b}");
}

/// `getrusage` (`RUSAGE_SELF`): Linux reports no `ixRSS`, `idRSS`, `isRSS`,
/// `nSwap`, `msgSent`, `msgRecv` or `signals`; lean (uv_system):
/// `true 0 0 0 0 0 0 0`.
#[test]
fn io_getrusage() {
    let r = getrusage().unwrap();
    assert!(r.max_rss > 0);
    assert_eq!((r.ix_rss, r.id_rss, r.is_rss, r.n_swap), (0, 0, 0, 0));
    assert_eq!((r.msg_sent, r.msg_recv, r.signals), (0, 0, 0));
}

/// The memory queries against `/proc/meminfo` and the process's cgroup.
#[test]
fn io_memory() {
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap();
    let kib = |key: &str| -> u64 {
        let l = meminfo.lines().find(|l| l.starts_with(key)).unwrap();
        l[key.len()..]
            .trim()
            .trim_end_matches(" kB")
            .trim()
            .parse()
            .unwrap()
    };
    assert_eq!(total_memory(), kib("MemTotal:") * 1024);
    assert!(free_memory().abs_diff(kib("MemAvailable:") * 1024) < 64 << 20);
    let cg = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let c = constrained_memory();
    if let Some(path) = cg.strip_prefix("0::/") {
        let dir = format!("/sys/fs/cgroup/{}", path.trim_end_matches('\n'));
        let read = |f: &str| match std::fs::read_to_string(format!("{dir}/{f}")) {
            Ok(t) if t == "max\n" => u64::MAX,
            Ok(t) => t.trim().parse().unwrap_or(0),
            Err(_) => 0,
        };
        let (max, high) = (read("memory.max"), read("memory.high"));
        assert_eq!(
            c,
            if max == 0 || high == 0 {
                0
            } else {
                max.min(high)
            }
        );
    }
    let a = available_memory();
    assert!(a > 0 && a <= total_memory().max(c));
}

/// `scanf`'s `%lu` and the cgroup v1 controller search.
#[test]
fn scanning() {
    let mut i = 0;
    assert_eq!(scan_unsigned(b"  42 x", &mut i), Some(42));
    let mut i = 0;
    assert_eq!(scan_unsigned(b"-1", &mut i), Some(u64::MAX));
    let mut i = 0;
    assert_eq!(
        scan_unsigned(b"99999999999999999999999", &mut i),
        Some(u64::MAX)
    );
    let mut i = 0;
    assert_eq!(scan_unsigned(b"max\n", &mut i), None);
    assert_eq!(
        cgroup1_memory_controller(b"12:cpu:/a\n5:memory:/user.slice/x\n1:name=systemd:/"),
        Some(&b"user.slice/x"[..])
    );
    assert_eq!(cgroup1_memory_controller(b"0::/user.slice"), None);
}

/// `cpuInfo` (`uv_cpu_info`): one entry per `cpuN` line of `/proc/stat`,
/// times ten times its ticks; lean (uv_system, RtSystem): every model
/// `unknown` on this host's AArch64 parts (Cortex-X925 and A725, which libuv
/// 1.48's table lacks).
#[test]
fn io_cpu_info() {
    let stat = std::fs::read_to_string("/proc/stat").unwrap();
    let lines = stat
        .lines()
        .skip(1)
        .take_while(|l| l.starts_with("cpu"))
        .count();
    let cpus = cpu_info().unwrap();
    assert_eq!(cpus.len(), lines);
    for c in &cpus {
        assert!(!c.model.is_empty());
        assert_eq!(c.times.user % 10, 0);
    }
}

/// `osGetPriority` (`uv_os_getpriority`): the nice value of `/proc/self/stat`
/// for pid 0, the process's pid and a pid whose low 32 bits are 0; lean
/// (uv_system): `ESRCH` for a missing pid and a negative C `int`.
#[test]
fn io_os_getpriority() {
    if child_case().as_deref() == Some("getpriority") {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let after = &stat[stat.rfind(')').unwrap() + 2..];
        let nice: i64 = after.split(' ').nth(16).unwrap().parse().unwrap();
        let own = u64::from(std::process::id());
        for pid in [0, own, 1 << 32] {
            assert_eq!(os_getpriority(pid).unwrap(), nice, "{pid}");
        }
        return;
    }
    assert_eq!(os_getpriority(i32::MAX as u64).unwrap_err(), esrch());
    assert_eq!(os_getpriority(u64::from(u32::MAX)).unwrap_err(), esrch());
    // the main thread's nice value is the process's: a child
    ok(child("io_os_getpriority", "getpriority"));
}

/// `osSetPriority` (`uv_os_setpriority`); lean (uv_system): 20 and -21 are
/// `EINVAL` before any call, a missing pid `ESRCH`; the priority is a C
/// `int` (`2^32 + 19` is 19). The change runs in a child.
#[test]
fn io_os_setpriority() {
    if child_case().as_deref() == Some("setpriority") {
        let now = os_getpriority(0).unwrap();
        os_setpriority(0, now).unwrap();
        os_setpriority(0, (1 << 32) + 19).unwrap();
        assert_eq!(os_getpriority(0).unwrap(), 19);
        return;
    }
    assert_eq!(os_setpriority(0, 20).unwrap_err(), einval());
    assert_eq!(os_setpriority(0, -21).unwrap_err(), einval());
    assert_eq!(
        os_setpriority(0, 4294967296 * 5 - 30).unwrap_err(),
        einval()
    );
    assert_eq!(os_setpriority(i32::MAX as u64, 1).unwrap_err(), esrch());
    ok(child("io_os_setpriority", "setpriority"));
}

/// `random`: more than `0x7FFFFFFF` bytes is `E2BIG` at once
/// (`resource exhausted (error code: 7, argument list too long)`).
#[test]
fn io_random() {
    assert_eq!(
        random_check(0x8000_0000).unwrap_err(),
        IoError::ResourceExhausted(None, 7, "argument list too long".to_owned())
    );
    random_check(0x7FFF_FFFF).unwrap();
    let mut b = [0u8; 64];
    random_fill(&mut b).unwrap();
    assert!(b.iter().any(|&x| x != 0));
}

/// The modelled `errno` (review RIO2-02; native case `uvsys/errno_after`):
/// libuv clears it before `getpriority`, glibc's `getpwuid_r` and
/// `getgrgid_r` leave their result (0), a failing system call sets it, an
/// error libuv makes itself leaves it.
#[test]
fn errno_model() {
    use crate::io::error::{errno, set_errno};
    set_errno(9);
    os_getpriority(0).unwrap();
    assert_eq!(errno(), 0);
    set_errno(9);
    os_get_passwd().unwrap();
    assert_eq!(errno(), 0);
    set_errno(9);
    os_get_group(u64::from(nix::unistd::Gid::effective().as_raw())).unwrap();
    assert_eq!(errno(), 0);
    set_errno(9);
    assert_eq!(os_setpriority(0, 20).unwrap_err(), einval());
    assert_eq!(errno(), 9);
    random_check(1 << 31).unwrap_err();
    assert_eq!(errno(), 9);
    assert_eq!(os_getpriority(u64::from(u32::MAX)).unwrap_err(), esrch());
    assert_eq!(errno(), 3);
    set_errno(9);
    let mut p = Vec::new();
    crate::io::temp::create_temp_dir(&mut p).unwrap();
    assert_eq!(errno(), 0);
    std::fs::remove_dir(std::ffi::OsStr::from_bytes(&p)).unwrap();
}

/// `/proc/stat` for CPUs 0 to 3 and 5 (4 offline), ticks 1 to 6 per field.
const STAT: &[u8] = b"cpu  100 0 0 0 0 0 0 0 0 0\n\
cpu0 1 2 3 4 5 6 7 0 0 0\n\
cpu1 11 12 13 14 15 16 17 0 0 0\n\
cpu2 21 22 23 24 25 26 27 0 0 0\n\
cpu3 31 32 33 34 35 36 37 0 0 0\n\
cpu5 51 52 53 54 55 56 57 0 0 0\n\
intr 12345 0 0\nctxt 999\n";

fn models(v: &[CpuInfo]) -> Vec<String> {
    v.iter()
        .map(|c| String::from_utf8(c.model.clone()).unwrap())
        .collect()
}

/// An AArch64 `/proc/cpuinfo` block per CPU with the given part code.
fn aarch64_info(parts: &[&str]) -> Vec<u8> {
    let mut s = String::new();
    for (i, p) in parts.iter().enumerate() {
        let cpu = if i == 4 { 5 } else { i };
        s.push_str(&format!(
            "processor\t: {cpu}\nBogoMIPS\t: 2000.00\nFeatures\t: fp asimd\nCPU implementer\t: 0x41\n\
             CPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: {p}\nCPU revision\t: 1\n\n"
        ));
    }
    s.into_bytes()
}

/// `uv_cpu_info` over fixtures (review RIO2-09): the CPUs of `/proc/stat` in
/// order, an offline one absent, times ten times the ticks (irq is the sixth
/// field), the speed in MHz from `scaling_cur_freq` (0 without it, an error
/// for a file without a number, where libuv aborts).
#[test]
fn cpu_info_stat_and_speed() {
    let info = aarch64_info(&["0xd0c", "0xd0c", "0xd0c", "0xd0c", "0xd0c"]);
    let freq = |cpu: usize| (cpu != 2).then(|| format!("{}\n", 1_000_000 + cpu).into_bytes());
    let v = parse_cpu_info(STAT, Some(&info), AARCH64_MARKER, AARCH64_PARTS, freq).unwrap();
    assert_eq!(v.len(), 5);
    assert_eq!(
        v[1].times,
        CpuTimes {
            user: 110,
            nice: 120,
            sys: 130,
            idle: 140,
            irq: 160
        }
    );
    let speeds: Vec<u64> = v.iter().map(|c| c.speed).collect();
    assert_eq!(speeds, [1000, 1000, 0, 1000, 1000]);
    assert_eq!(models(&v), ["Neoverse-N1"; 5]);
    let bad = |_: usize| Some(b"x\n".to_vec());
    assert!(parse_cpu_info(STAT, Some(&info), AARCH64_MARKER, AARCH64_PARTS, bad).is_err());
    // without /proc/cpuinfo every model is libuv's "unknown"
    let v = parse_cpu_info(STAT, None, AARCH64_MARKER, AARCH64_PARTS, |_| None).unwrap();
    assert_eq!(models(&v), ["unknown"; 5]);
    // a /proc/stat without a line is libuv's abort: here an error
    assert!(parse_cpu_info(b"", None, AARCH64_MARKER, AARCH64_PARTS, |_| None).is_err());
}

/// libuv's model slots (a quirk the runtime keeps): slot 0 is "unknown", an
/// empty slot matches any name, and the slot after the first match takes the
/// name if it is empty and names the CPU. So while slot 1 is empty, every
/// name lands in slot 2 and the first one sticks: 0xd0c is Neoverse-N1, and
/// 0xd0b (Cortex-A76) then reads Neoverse-N1. A part missing from the table
/// is "unknown", which fills slot 1; after it the empty slot moves on, so
/// Neoverse-N1 fills slot 3 and Cortex-A76 slot 5.
#[test]
fn cpu_info_aarch64_slots() {
    let info = aarch64_info(&["0xd0c", "0xd0b", "0xfff", "0xd0c", "0xd0b"]);
    let v = parse_cpu_info(STAT, Some(&info), AARCH64_MARKER, AARCH64_PARTS, |_| None).unwrap();
    assert_eq!(
        models(&v),
        [
            "Neoverse-N1",
            "Neoverse-N1",
            "unknown",
            "Neoverse-N1",
            "Cortex-A76"
        ]
    );
    let info = aarch64_info(&["0xfff", "0xd03", "0xd05", "0xd03", "0xfff"]);
    let v = parse_cpu_info(STAT, Some(&info), AARCH64_MARKER, AARCH64_PARTS, |_| None).unwrap();
    assert_eq!(
        models(&v),
        [
            "unknown",
            "Cortex-A53",
            "Cortex-A53",
            "Cortex-A53",
            "unknown"
        ]
    );
}

/// x86-64's marker, names taken as they are (no part table), and more than
/// eight distinct models: libuv's eight slots never fill, as above.
#[test]
fn cpu_info_x86_lines() {
    let mut s = String::new();
    for (i, cpu) in [0, 1, 2, 3, 5].iter().enumerate() {
        s.push_str(&format!(
            "processor\t: {cpu}\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: {}\n\
             model name\t: Intel(R) Xeon(R) CPU model {i}\nstepping\t: 7\ncpu MHz\t\t: 2100.000\n\n",
            100 + i
        ));
    }
    let v = parse_cpu_info(STAT, Some(s.as_bytes()), X86_MARKER, b"", |_| None).unwrap();
    assert_eq!(models(&v), ["Intel(R) Xeon(R) CPU model 0"; 5]);
    // more than eight names, and a name longer than 63 bytes cut there
    let long = "L".repeat(80);
    let mut s = format!("processor\t: 0\nmodel name\t: {long}\n\n");
    for i in 1..12 {
        s.push_str(&format!("processor\t: {i}\nmodel name\t: model {i}\n\n"));
    }
    let stat: String = std::iter::once("cpu  0 0 0 0 0 0 0\n".to_owned())
        .chain((0..12).map(|i| format!("cpu{i} 1 1 1 1 1 1 1\n")))
        .collect();
    let v = parse_cpu_info(stat.as_bytes(), Some(s.as_bytes()), X86_MARKER, b"", |_| {
        None
    })
    .unwrap();
    assert_eq!(v.len(), 12);
    assert!(models(&v).iter().all(|m| *m == "L".repeat(63)));
}
