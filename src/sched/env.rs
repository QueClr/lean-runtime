//! What the task manager reads from the environment: the number of worker
//! threads (`LEAN_NUM_THREADS`, else the number of online processors) and the
//! stack size of a thread (`LEAN_STACK_SIZE_KB`, else 1 GiB). From lean2rr's
//! leanrt (`sched.rs`: `pool_limit`, `hardware_concurrency`; `rt.rs`:
//! `main_stack_size`, `strtoull10`), with `sysconf` replaced by the file glibc
//! reads for it.

/// The number of worker threads of Lean's task manager
/// (`get_lean_num_threads` in `src/runtime/object.cpp`): `LEAN_NUM_THREADS`
/// read with C's `atoi`, or `std::thread::hardware_concurrency()`. Lean
/// passes it to `lean_init_task_manager_using(unsigned)`: 0 means no task
/// manager (tasks run at once, as during initialization), and a negative
/// number wraps.
pub fn lean_num_threads() -> u32 {
    match std::env::var_os("LEAN_NUM_THREADS") {
        Some(v) => atoi_unsigned(std::os::unix::ffi::OsStrExt::as_bytes(v.as_os_str())),
        None => hardware_concurrency(),
    }
}

/// glibc's `atoi(s)`, which is `(int) strtol(s, NULL, 10)`, taken as an
/// `unsigned`: leading white space, an optional sign, decimal digits up to the
/// first other character; `strtol` saturates at the bounds of a `long`, and
/// the cast keeps the low 32 bits.
pub(crate) fn atoi_unsigned(s: &[u8]) -> u32 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
        i += 1;
    }
    let neg = i < s.len() && s[i] == b'-';
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    let mut n: i128 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        n = (n * 10 + (s[i] - b'0') as i128).min(i64::MAX as i128 + 1);
        i += 1;
    }
    let n = if neg { -n } else { n }.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    n as i32 as u32
}

/// `std::thread::hardware_concurrency()` as Lean's runtime gets it (libc++
/// calls `sysconf(_SC_NPROCESSORS_ONLN)`): the number of online processors,
/// not limited by the CPU affinity mask or a cgroup quota; 0 if unknown.
/// glibc counts them from `/sys/devices/system/cpu/online` (a list of ranges
/// such as `0-3,8-11`), and falls back to `/proc/stat`.
pub fn hardware_concurrency() -> u32 {
    if let Some(s) = first_line("/sys/devices/system/cpu/online") {
        if let Some(n) = count_cpu_list(s.trim()) {
            return n;
        }
    }
    if let Ok(s) = std::fs::read_to_string("/proc/stat") {
        let n = s
            .lines()
            .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
            .count();
        if n > 0 {
            return n as u32;
        }
    }
    0
}

/// The first line of the file at `path`, read as glibc's `get_nprocs`
/// reads `/sys/devices/system/cpu/online` (review AR-31): `openat` with
/// `O_RDONLY | O_CLOEXEC`, `read`s of 1024 bytes until a newline or the end
/// (one, for a CPU list), then `close`. No `statx`: std's `read_to_string`
/// makes one for its size hint, which native's startup does not.
fn first_line(path: &str) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(k) = buf[..n].iter().position(|&b| b == b'\n') {
                    line.extend_from_slice(&buf[..k]);
                    break;
                }
                line.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    String::from_utf8(line).ok()
}

/// The number of processors in a kernel CPU list (`0-3,8,10-11`).
pub(crate) fn count_cpu_list(s: &str) -> Option<u32> {
    let mut n: u32 = 0;
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
                if b < a {
                    return None;
                }
                n = n.checked_add(b - a + 1)?;
            }
            None => {
                part.parse::<u32>().ok()?;
                n = n.checked_add(1)?;
            }
        }
    }
    if n == 0 {
        None
    } else {
        Some(n)
    }
}

/// The stack size of Lean's threads (`lthread::m_thread_stack_size`,
/// `src/runtime/thread.cpp`): 1 GiB on 64-bit targets, or, when
/// `LEAN_STACK_SIZE_KB` is set (`set_thread_stack_size_from_env`, called by
/// `lean_run_main`), that many KiB rounded down to a multiple of 4, plus
/// `LEAN_STACK_BUFFER_SPACE` (128 KiB). The task manager's workers get this
/// size, and so does each context of the scheduler.
pub fn thread_stack_size() -> usize {
    if let Some(v) = std::env::var_os("LEAN_STACK_SIZE_KB") {
        let kb = strtoull10(std::os::unix::ffi::OsStrExt::as_bytes(v.as_os_str()));
        let sz = (kb / 4 * 4).wrapping_mul(1024) as usize;
        if sz > 0 {
            return sz.saturating_add(STACK_BUFFER_SPACE);
        }
    }
    DEFAULT_THREAD_STACK
}

/// `lthread`'s default stack size on 64-bit targets (1 GiB), which
/// `LEAN_STACK_SIZE_KB` replaces once `lean_run_main` has read it. libuv's
/// loop thread is made before that (`libuv.cpp`), so it keeps this size
/// whatever the variable says (hunt HSK-03).
pub(crate) const DEFAULT_THREAD_STACK: usize = 1 << 30;

/// `LEAN_STACK_BUFFER_SPACE` (`src/runtime/thread.h`).
pub(crate) const STACK_BUFFER_SPACE: usize = 128 * 1024;

/// C's `strtoull(s, nullptr, 10)`: leading white space, an optional sign
/// (`-` negates modulo 2^64), then decimal digits up to the first other
/// character; saturates at 2^64 - 1.
pub(crate) fn strtoull10(s: &[u8]) -> u64 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
        i += 1;
    }
    let neg = i < s.len() && s[i] == b'-';
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    let mut n: u64 = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        match n
            .checked_mul(10)
            .and_then(|m| m.checked_add((s[i] - b'0') as u64))
        {
            Some(m) => n = m,
            None => overflow = true,
        }
        i += 1;
    }
    if overflow {
        return u64::MAX;
    }
    if neg {
        n.wrapping_neg()
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_as_lean_reads_it() {
        assert_eq!(atoi_unsigned(b"4"), 4);
        assert_eq!(atoi_unsigned(b"  +12abc"), 12);
        assert_eq!(atoi_unsigned(b"abc"), 0);
        assert_eq!(atoi_unsigned(b""), 0);
        assert_eq!(atoi_unsigned(b"-1"), u32::MAX);
        // strtol saturates at LONG_MAX, whose low 32 bits are all ones.
        assert_eq!(atoi_unsigned(b"99999999999999999999999"), u32::MAX);
        // 2^32 + 3 keeps its low 32 bits.
        assert_eq!(atoi_unsigned(b"4294967299"), 3);
        assert_eq!(atoi_unsigned(b"\t-2"), u32::MAX - 1);
    }

    #[test]
    fn first_lines() {
        let dir = std::env::temp_dir().join(format!("lean-runtime-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("online");
        std::fs::write(&f, "0-19\n").unwrap();
        assert_eq!(first_line(f.to_str().unwrap()).as_deref(), Some("0-19"));
        // longer than one read, and no newline at the end
        let long: String = (0..400).map(|k| format!("{},", 2 * k)).collect();
        std::fs::write(&f, long.as_bytes()).unwrap();
        assert_eq!(first_line(f.to_str().unwrap()), Some(long.clone()));
        std::fs::write(&f, format!("{long}\nsecond\n")).unwrap();
        assert_eq!(first_line(f.to_str().unwrap()), Some(long));
        assert_eq!(first_line(dir.join("absent").to_str().unwrap()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cpu_lists() {
        assert_eq!(count_cpu_list("0-3"), Some(4));
        assert_eq!(count_cpu_list("0-3,8-11"), Some(8));
        assert_eq!(count_cpu_list("0,2,4"), Some(3));
        assert_eq!(count_cpu_list("0"), Some(1));
        assert_eq!(count_cpu_list(""), None);
        assert_eq!(count_cpu_list("3-1"), None);
        assert_eq!(count_cpu_list("x"), None);
    }

    #[test]
    fn strtoull_as_c() {
        assert_eq!(strtoull10(b"64"), 64);
        assert_eq!(strtoull10(b" 7kb"), 7);
        assert_eq!(strtoull10(b"-1"), u64::MAX);
        assert_eq!(strtoull10(b"99999999999999999999999"), u64::MAX);
        assert_eq!(strtoull10(b"x"), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn hardware_concurrency_counts_something() {
        assert!(hardware_concurrency() >= 1);
    }
}
