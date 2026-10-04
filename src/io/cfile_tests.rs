//! Unit tests of the FILE model's hooks for the other io modules. The model
//! itself is tested against glibc in `tests/cfile_glibc.rs` and against
//! native Lean in `tests/io_rows.rs` and `tests/io_cases.rs`.

use super::*;
use crate::io::{Handle, IoError};
use rustix::fs::OFlags;

/// A pipe; every test writes at most a few bytes into it before reading, far
/// below one page, the smallest capacity the kernel gives (a user over
/// `fs.pipe-user-pages-soft` gets one-page pipes), or writes non-blocking.
fn pipe() -> (OwnedFd, OwnedFd) {
    let (r, w) = std::io::pipe().unwrap();
    (r.into(), w.into())
}

/// `pending_output` is the put area's unwritten bytes, empty after a flush
/// and outside put mode. It and the open-list tests also run under Miri
/// (`doallocate` asks no `fstat` there).
#[test]
fn pending_output_is_the_put_area() {
    let (r, w) = pipe();
    let mut f = CFile::fdopen(w, FsMode::Write);
    assert_eq!(f.pending_output(), b"");
    f.put(b"abc").unwrap();
    assert_eq!(f.pending_output(), b"abc");
    f.put(b"de").unwrap();
    assert_eq!(f.pending_output(), b"abcde");
    f.flush().unwrap();
    assert_eq!(f.pending_output(), b"");
    let mut got = [0u8; 8];
    let n = rustix::io::read(&r, &mut got[..]).unwrap();
    assert_eq!(&got[..n], b"abcde");
    // a reading stream has none
    let (r2, w2) = pipe();
    rustix::io::write(&w2, b"xy\n").unwrap();
    drop(w2);
    let mut g = CFile::fdopen(r2, FsMode::Read);
    let mut line = Vec::new();
    g.get_line(&mut line).unwrap();
    assert_eq!(g.pending_output(), b"");
}

/// A bounded pipe whose reader never reads: once full, a write fails with
/// `EPIPE` (resource vanished) where the descriptor says `EAGAIN`.
#[test]
#[cfg_attr(miri, ignore)]
fn bounded_pipe_reports_epipe() {
    let (_r, w) = pipe();
    rustix::fs::fcntl_setfl(&w, OFlags::NONBLOCK).unwrap();
    // more than the pipe holds, whatever its capacity
    let cap = rustix::pipe::fcntl_getpipe_size(&w).unwrap();
    let h = Handle::fdopen_bounded_pipe(w);
    let big = vec![b'x'; cap + 4096];
    assert_eq!(
        h.write(&big),
        Err(IoError::ResourceVanished(32, "broken pipe".into()))
    );
    // an ordinary non-blocking pipe reports EAGAIN
    let (_r2, w2) = pipe();
    rustix::fs::fcntl_setfl(&w2, OFlags::NONBLOCK).unwrap();
    let cap = rustix::pipe::fcntl_getpipe_size(&w2).unwrap();
    let big = vec![b'x'; cap + 4096];
    let h2 = Handle::fdopen(w2, FsMode::Write);
    assert_eq!(
        h2.write(&big),
        Err(IoError::ResourceExhausted(
            None,
            11,
            "resource temporarily unavailable".into()
        ))
    );
}

/// Reads `r` to its end (every write end closed).
fn read_to_end(r: &OwnedFd) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 16];
    loop {
        let n = rustix::io::read(r, &mut buf[..]).unwrap();
        if n == 0 {
            return got;
        }
        got.extend_from_slice(&buf[..n]);
    }
}

/// Taken by the tests that walk the open-handle list: a walk holds a
/// reference to every open file, which another such test would see in its
/// counts (no other test in the crate walks the list).
static OPEN_LIST_WALKS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn walks_alone() -> std::sync::MutexGuard<'static, ()> {
    OPEN_LIST_WALKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The open-handle list: newest first; it holds each file besides its
/// handles, and a handle leaves it when its last clone goes away (the file
/// then closes, its pending output written).
#[test]
fn open_list_order_and_close() {
    let _alone = walks_alone();
    let (r1, w1) = pipe();
    let (_r2, w2) = pipe();
    let a = Handle::fdopen(w1, FsMode::Write);
    let b = Handle::fdopen(w2, FsMode::Write);
    let fd_a = a.file().fileno();
    let fd_b = b.file().fileno();
    let order: Vec<i32> = crate::io::handle::open_files_newest_first()
        .iter()
        .map(|f| f.file.lock().unwrap().fileno())
        .filter(|&fd| fd == fd_a || fd == fd_b)
        .collect();
    assert_eq!(order, [fd_b, fd_a]);
    a.put_str(b"pending").unwrap();
    let count = |h: &Handle| std::sync::Arc::strong_count(h.file_stream().unwrap());
    // the list's slot and `a`
    assert_eq!(count(&a), 2);
    let a2 = a.clone();
    assert_eq!(count(&a2), 3);
    drop(a);
    assert_eq!(count(&a2), 2);
    drop(a2);
    // closed (so out of the list, which held it): its pending output written
    assert_eq!(read_to_end(&r1), b"pending");
}

/// A file whose last handle goes away while the exit holds the open files
/// (`open_files_newest_first`) closes when the exit lets go of them.
#[test]
fn open_list_release_after_walk() {
    let _alone = walks_alone();
    let (r, w) = pipe();
    let a = Handle::fdopen(w, FsMode::Write);
    a.put_str(b"late").unwrap();
    let open = crate::io::handle::open_files_newest_first();
    assert_eq!(std::sync::Arc::strong_count(a.file_stream().unwrap()), 3);
    drop(a);
    // still open: nothing written, no end of file
    rustix::fs::fcntl_setfl(&r, OFlags::NONBLOCK).unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(
        rustix::io::read(&r, &mut buf[..]),
        Err(rustix::io::Errno::AGAIN)
    );
    rustix::fs::fcntl_setfl(&r, OFlags::empty()).unwrap();
    drop(open);
    assert_eq!(read_to_end(&r), b"late");
}

/// glibc's `_IO_file_doallocate`: `st_blksize` only when positive and below
/// `BUFSIZ` (8192). From leanrs's `block.rs` test.
#[test]
fn buffer_size_follows_filedoalloc() {
    assert_eq!(buffer_size(0), 8192);
    assert_eq!(buffer_size(-1), 8192);
    assert_eq!(buffer_size(512), 512);
    assert_eq!(buffer_size(4096), 4096);
    assert_eq!(buffer_size(8191), 8191);
    assert_eq!(buffer_size(8192), 8192);
    assert_eq!(buffer_size(65536), 8192);
    assert_eq!(buffer_size(4 << 20), 8192);
    assert_eq!(buffer_size(i64::MAX), 8192);
}
