//! `/proc/<pid>/stat` read into a stack buffer and parsed with no
//! allocation, for the crate's ELF constructors (`argv_title`'s and
//! `startup_fds`'s, which must not use the global allocator: AR-36), and
//! the check they share: whether the crate's code lies in the program's own
//! executable (`start_code` to `end_code`), so a constructor acts only
//! there, never in a shared library (review RSH2-02). Safe code only.

use rustix::fd::BorrowedFd;
use std::ffi::CStr;

/// One page: more than a `/proc/<pid>/stat` line holds (52 fields, at most
/// 20 digits each, and a name of at most 64 bytes).
pub(crate) const STAT_BUF: usize = 4096;

/// The start of the `/proc` file `path` (relative to `dir` if given), read
/// into `buf` with safe calls and no allocation (AR-36): `None` when it
/// cannot be opened or read, or fills `buf` (then it may be cut).
pub(crate) fn read_proc<'a>(
    dir: Option<BorrowedFd<'_>>,
    path: &CStr,
    buf: &'a mut [u8],
) -> Option<&'a [u8]> {
    use rustix::fs::{openat, Mode, OFlags, CWD};
    let flags = OFlags::RDONLY | OFlags::CLOEXEC;
    let fd = match dir {
        Some(d) => openat(d, path, flags, Mode::empty()),
        None => openat(CWD, path, flags, Mode::empty()),
    }
    .ok()?;
    let mut n = 0;
    while n < buf.len() {
        match rustix::io::read(&fd, &mut buf[n..]) {
            Ok(0) => return Some(&buf[..n]),
            Ok(k) => n += k,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return None,
        }
    }
    None
}

/// The fields after `comm` of a `/proc/<pid>/stat` line, from field 3 on
/// (`comm` may hold spaces and `)`: it ends at the last `)`).
fn stat_fields(stat: &[u8]) -> impl Iterator<Item = &[u8]> {
    let rest = match stat.iter().rposition(|&b| b == b')') {
        Some(at) => &stat[at + 1..],
        None => &[][..],
    };
    rest.split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
}

/// Field `n` (from 1) of a `/proc/<pid>/stat` line, as a number.
pub(crate) fn stat_field(stat: &[u8], n: usize) -> Option<usize> {
    let f = stat_fields(stat).nth(n.checked_sub(3)?)?;
    std::str::from_utf8(f).ok()?.parse().ok()
}

/// Whether `code`, an address in this crate's code, lies in the program's
/// own executable rather than in a shared library: inside the kernel's
/// record of the program's code, `start_code` to `end_code`
/// (`/proc/self/stat`, fields 26 and 27; leanrs's review of quirks-1:
/// comparing the device and inode of `/proc/self/maps` and
/// `/proc/self/exe` fails on btrfs and older overlayfs). `None` when that
/// cannot be told (no `/proc`, the fields hidden).
///
/// A program started through the dynamic loader (`ld.so ./prog`) reads as
/// not in the executable: the kernel's record is then the loader's code. That
/// launch keeps "no arguments" (`ENOBUFS`), where native writes the title (a
/// judged deviation, LQ1-01, `docs/native-quirks.md`), and its startup
/// descriptors open in `main`, not in the constructor.
pub(crate) fn in_main_executable(stat: &[u8], code: usize) -> Option<bool> {
    code_span_of(stat).map(|(start, end)| (start..end).contains(&code))
}

/// [`in_main_executable`], reading `/proc/self/stat` itself (one
/// descriptor, open only during the read).
#[cfg(feature = "startup-fds")]
pub(crate) fn code_in_main_executable(code: usize) -> Option<bool> {
    let mut buf = [0u8; STAT_BUF];
    in_main_executable(read_proc(None, c"/proc/self/stat", &mut buf)?, code)
}

/// `start_code` and `end_code` of a `/proc/<pid>/stat` line (fields 26 and
/// 27); `None` when they are missing, or hidden from a reader without
/// permission (the kernel then shows both as 1: an empty span).
pub(crate) fn code_span_of(stat: &[u8]) -> Option<(usize, usize)> {
    let (start, end) = (stat_field(stat, 26)?, stat_field(stat, 27)?);
    (start < end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_after_comm() {
        // `comm` with a space and a `)`
        let mut line = b"1234 (a b) c) S".to_vec();
        for n in 4..=52 {
            line.extend_from_slice(format!(" {}", n * 10).as_bytes());
        }
        assert_eq!(stat_field(&line, 4), Some(40));
        assert_eq!(stat_field(&line, 9), Some(90));
        assert_eq!(code_span_of(&line), Some((260, 270)));
        assert_eq!(code_span_of(b"1 (x) S 1 2"), None);
        assert_eq!(stat_field(b"no comm here", 3), None);
    }

    /// The crate's code is in this test executable; the stack is not; a
    /// buffer the read fills is no answer.
    #[test]
    #[cfg_attr(miri, ignore)] // `/proc` is not readable under Miri
    fn this_process() {
        let mut buf = [0u8; STAT_BUF];
        let stat = read_proc(None, c"/proc/self/stat", &mut buf).unwrap();
        let code = (this_process as *const ()).addr();
        assert_eq!(in_main_executable(stat, code), Some(true));
        let local = 0u8;
        assert_eq!(
            in_main_executable(stat, std::ptr::addr_of!(local).addr()),
            Some(false)
        );
        let mut small = [0u8; 8];
        assert_eq!(read_proc(None, c"/proc/self/stat", &mut small), None);
        #[cfg(feature = "startup-fds")]
        assert_eq!(code_in_main_executable(code), Some(true));
    }
}
