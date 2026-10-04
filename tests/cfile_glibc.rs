//! Differential tests of the `FILE` model (`lean_runtime::io::cfile`):
//! random sequences of Lean's handle operations run on a real glibc `FILE`
//! and on a `CFile`, each on its own copy of a file (or its own pipe); after
//! every step the results (with the `errno` an error reports), `feof` and the
//! bytes on disk must agree.
//!
//! The oracle is glibc with one change, LB-02 (`docs/lean-bugs.md`): before a
//! read of at least one buffer while output is pending (`__fpending`), it
//! calls `fflush`, as the model does, since glibc itself would drop that
//! output.
//!
//! From lean2rr's `runtime/leanrt/src/cfile_tests.rs`. This test calls glibc
//! through `extern "C"` declarations, which needs `unsafe`; it is test code
//! only, the oracle, and never part of the library.
#![cfg(all(feature = "io", not(miri)))]

use lean_runtime::io::cfile::CFile;
use lean_runtime::io::FsMode;
use std::ffi::{c_char, c_int, c_long, c_void, CString};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
type FILE = c_void;

extern "C" {
    fn fdopen(fd: c_int, mode: *const c_char) -> *mut FILE;
    fn fclose(f: *mut FILE) -> c_int;
    fn fwrite(p: *const c_void, s: usize, n: usize, f: *mut FILE) -> usize;
    fn fread(p: *mut c_void, s: usize, n: usize, f: *mut FILE) -> usize;
    fn fflush(f: *mut FILE) -> c_int;
    fn fseek(f: *mut FILE, off: c_long, whence: c_int) -> c_int;
    fn ftello(f: *mut FILE) -> i64;
    fn getc(f: *mut FILE) -> c_int;
    fn feof(f: *mut FILE) -> c_int;
    fn ferror(f: *mut FILE) -> c_int;
    fn clearerr(f: *mut FILE);
    fn fileno(f: *mut FILE) -> c_int;
    fn ftruncate(fd: c_int, len: i64) -> c_int;
    fn __fpending(f: *mut FILE) -> usize;
    fn __errno_location() -> *mut c_int;
}

fn c_errno() -> i32 {
    unsafe { *__errno_location() }
}

/// Lean's operations on a glibc `FILE` (`lean_io_prim_handle_*`).
struct Glibc {
    f: *mut FILE,
    /// The `FILE`'s buffer size: `st_blksize`, at most `BUFSIZ`.
    bufsize: usize,
}

impl Glibc {
    fn new(fd: OwnedFd, mode: &str) -> Glibc {
        let file = std::fs::File::from(fd);
        let blk = file.metadata().map(|m| m.blksize() as usize).unwrap_or(0);
        let bufsize = if blk > 0 && blk < 8192 { blk } else { 8192 };
        let cm = CString::new(mode).unwrap();
        let f = unsafe { fdopen(file.into_raw_fd(), cm.as_ptr()) };
        assert!(!f.is_null());
        Glibc { f, bufsize }
    }
    fn put(&mut self, d: &[u8]) -> Result<(), i32> {
        if d.is_empty() {
            return Ok(());
        }
        let m = unsafe { fwrite(d.as_ptr() as *const c_void, 1, d.len(), self.f) };
        if m == d.len() {
            Ok(())
        } else {
            Err(c_errno())
        }
    }
    fn flush(&mut self) -> Result<(), i32> {
        if unsafe { fflush(self.f) } == 0 {
            Ok(())
        } else {
            Err(c_errno())
        }
    }
    fn read(&mut self, n: usize) -> Result<Vec<u8>, i32> {
        let mut v = vec![0u8; n];
        if n == 0 {
            return Ok(Vec::new());
        }
        // LB-02: the pending output is written before a direct read
        let flushed = !(n >= self.bufsize && unsafe { __fpending(self.f) } > 0)
            || unsafe { fflush(self.f) } == 0;
        let got = if flushed {
            unsafe { fread(v.as_mut_ptr() as *mut c_void, 1, n, self.f) }
        } else {
            0
        };
        if got > 0 {
            v.truncate(got);
            Ok(v)
        } else if unsafe { feof(self.f) } != 0 {
            unsafe { clearerr(self.f) };
            Ok(Vec::new())
        } else {
            Err(c_errno())
        }
    }
    fn get_line(&mut self) -> Result<Vec<u8>, i32> {
        let mut l = Vec::new();
        loop {
            let c = unsafe { getc(self.f) };
            if c == -1 {
                break;
            }
            l.push(c as u8);
            if c == b'\n' as c_int {
                break;
            }
        }
        if unsafe { ferror(self.f) } != 0 {
            return Err(c_errno());
        }
        if unsafe { feof(self.f) } != 0 {
            unsafe { clearerr(self.f) };
        }
        Ok(l)
    }
    fn rewind(&mut self) -> Result<(), i32> {
        if unsafe { fseek(self.f, 0, 0) } == 0 {
            Ok(())
        } else {
            Err(c_errno())
        }
    }
    fn truncate(&mut self) -> Result<(), i32> {
        let pos = unsafe { ftello(self.f) };
        if unsafe { ftruncate(fileno(self.f), pos) } == 0 {
            Ok(())
        } else {
            Err(c_errno())
        }
    }
    fn is_eof(&self) -> bool {
        unsafe { feof(self.f) != 0 }
    }
}

impl Drop for Glibc {
    fn drop(&mut self) {
        unsafe { fclose(self.f) };
    }
}

/// The model's operations, with the same result shapes.
struct Model(CFile);

impl Model {
    fn read(&mut self, n: usize) -> Result<Vec<u8>, i32> {
        let mut v = vec![0u8; n];
        let got = self.0.read(&mut v)?;
        v.truncate(got);
        Ok(v)
    }
    fn get_line(&mut self) -> Result<Vec<u8>, i32> {
        let mut l = Vec::new();
        self.0.get_line(&mut l).map(|()| l)
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn file_bytes(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

/// Open `path` with Lean's flags for `mode`.
fn open_mode(path: &str, mode: FsMode) -> OwnedFd {
    let mut o = std::fs::OpenOptions::new();
    match mode {
        FsMode::Read => o.read(true),
        FsMode::Write => o.write(true).create(true).truncate(true),
        FsMode::ReadWrite => o.read(true).write(true),
        _ => o.append(true).create(true),
    };
    o.mode(0o666).open(path).unwrap().into()
}

fn c_mode(mode: FsMode) -> &'static str {
    match mode {
        FsMode::Read => "r",
        FsMode::Write | FsMode::WriteNew => "w",
        FsMode::ReadWrite => "r+",
        FsMode::Append => "a",
    }
}

fn run_case(seed: u64, mode: FsMode, steps: usize) {
    let dir = std::env::temp_dir();
    let tag = format!(
        "lean-runtime-cfile-{}-{}-{:?}",
        std::process::id(),
        seed,
        mode
    );
    let pa = dir.join(format!("{tag}-a")).to_string_lossy().into_owned();
    let pb = dir.join(format!("{tag}-b")).to_string_lossy().into_owned();
    let mut rng = Rng(seed * 2654435761 + 12345);
    // Initial contents: lines of varied length, some large.
    let mut init = Vec::new();
    for i in 0..rng.below(40) {
        let len = if rng.below(8) == 0 {
            rng.below(9000)
        } else {
            rng.below(80)
        };
        for j in 0..len {
            init.push(b'a' + ((i + j) % 26) as u8);
        }
        init.push(b'\n');
    }
    std::fs::write(&pa, &init).unwrap();
    std::fs::write(&pb, &init).unwrap();
    let mut g = Glibc::new(open_mode(&pa, mode), c_mode(mode));
    let mut m = Model(CFile::fdopen(open_mode(&pb, mode), mode));
    for step in 0..steps {
        let op = rng.below(8);
        let (ra, rb): (Result<Vec<u8>, i32>, Result<Vec<u8>, i32>) = match op {
            7 => {
                // Another writer changes both files the same way: stale
                // buffered data must be reused (or not) as by glibc.
                let at = rng.below(file_bytes(&pa).len() as u64 + 1);
                let len = rng.below(300) as usize + 1;
                let d: Vec<u8> = (0..len).map(|k| b'0' + ((k + step) % 10) as u8).collect();
                for p in [&pa, &pb] {
                    let mut f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
                    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(at)).unwrap();
                    std::io::Write::write_all(&mut f, &d).unwrap();
                }
                (Ok(Vec::new()), Ok(Vec::new()))
            }
            0 => {
                let len = match rng.below(4) {
                    0 => rng.below(10),
                    1 => rng.below(200),
                    2 => rng.below(5000),
                    _ => rng.below(20000),
                } as usize;
                let mut d: Vec<u8> = (0..len).map(|k| b'A' + ((k + step) % 26) as u8).collect();
                if len > 0 && rng.below(2) == 0 {
                    let at = rng.below(len as u64) as usize;
                    d[at] = b'\n';
                }
                (
                    g.put(&d).map(|_| Vec::new()),
                    m.0.put(&d).map(|_| Vec::new()),
                )
            }
            1 => {
                let n = match rng.below(3) {
                    0 => rng.below(20),
                    1 => rng.below(5000),
                    _ => rng.below(20000),
                } as usize;
                (g.read(n), m.read(n))
            }
            2 => (g.get_line(), m.get_line()),
            3 => (
                g.flush().map(|_| Vec::new()),
                m.0.flush().map(|_| Vec::new()),
            ),
            4 => (
                g.rewind().map(|_| Vec::new()),
                m.0.rewind().map(|_| Vec::new()),
            ),
            5 => (
                g.truncate().map(|_| Vec::new()),
                m.0.truncate().map(|_| Vec::new()),
            ),
            _ => {
                let a = unsafe { ftello(g.f) };
                let b = m.0.ftell();
                (Ok(a.to_le_bytes().to_vec()), Ok(b.to_le_bytes().to_vec()))
            }
        };
        let ctx = format!("seed {seed} mode {mode:?} step {step} op {op}");
        assert_eq!(ra, rb, "result differs: {ctx}");
        assert_eq!(g.is_eof(), m.0.is_eof(), "feof differs: {ctx}");
        assert_eq!(
            file_bytes(&pa),
            file_bytes(&pb),
            "file contents differ: {ctx}"
        );
    }
    drop(g);
    m.0.close();
    assert_eq!(
        file_bytes(&pa),
        file_bytes(&pb),
        "file contents differ after close: seed {seed} mode {mode:?}"
    );
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
}

/// A read-only stream on a pipe holding `data` (at most 64 KiB, so the
/// writes cannot block), write end closed: stdin from a pipe.
fn pipe_with(data: &[u8]) -> OwnedFd {
    let (r, mut w) = std::io::pipe().unwrap();
    std::io::Write::write_all(&mut w, data).unwrap();
    drop(w);
    r.into()
}

fn run_pipe_case(seed: u64, steps: usize) {
    let mut rng = Rng(seed * 40503 + 977);
    let len = rng.below(60000) as usize;
    let data: Vec<u8> = (0..len)
        .map(|k| {
            if rng.below(30) == 0 {
                b'\n'
            } else {
                b'a' + (k % 26) as u8
            }
        })
        .collect();
    let mut g = Glibc::new(pipe_with(&data), "r");
    let mut m = Model(CFile::fdopen(pipe_with(&data), FsMode::Read));
    for step in 0..steps {
        let op = rng.below(7);
        let (ra, rb): (Result<Vec<u8>, i32>, Result<Vec<u8>, i32>) = match op {
            0 => (
                g.put(b"x").map(|_| Vec::new()),
                m.0.put(b"x").map(|_| Vec::new()),
            ),
            1 => {
                let n = match rng.below(3) {
                    0 => rng.below(20),
                    1 => rng.below(5000),
                    _ => rng.below(20000),
                } as usize;
                (g.read(n), m.read(n))
            }
            2 | 3 => (g.get_line(), m.get_line()),
            4 => (
                g.flush().map(|_| Vec::new()),
                m.0.flush().map(|_| Vec::new()),
            ),
            5 => (
                g.rewind().map(|_| Vec::new()),
                m.0.rewind().map(|_| Vec::new()),
            ),
            _ => (
                g.truncate().map(|_| Vec::new()),
                m.0.truncate().map(|_| Vec::new()),
            ),
        };
        let ctx = format!("pipe seed {seed} step {step} op {op}");
        assert_eq!(ra, rb, "result differs: {ctx}");
        assert_eq!(g.is_eof(), m.0.is_eof(), "feof differs: {ctx}");
    }
}

#[test]
fn differential_against_glibc_pipes() {
    for seed in 1..=300u64 {
        run_pipe_case(seed, 60);
    }
}

#[test]
fn differential_against_glibc() {
    for seed in 1..=300u64 {
        for mode in [
            FsMode::Read,
            FsMode::Write,
            FsMode::ReadWrite,
            FsMode::Append,
        ] {
            run_case(seed, mode, 60);
        }
    }
}
