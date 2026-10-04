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
/// and outside put mode.
#[test]
#[cfg_attr(miri, ignore)]
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

/// The open-handle list: newest first, a handle leaves it when its last clone
/// goes away (and is closed, its pending output written).
#[test]
#[cfg_attr(miri, ignore)]
fn open_list_order_and_close() {
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
    let weak = std::sync::Arc::downgrade(a.file_stream().unwrap());
    let listed = |w: &std::sync::Weak<crate::io::handle::FileStream>| {
        crate::io::handle::OPEN
            .lock()
            .unwrap()
            .iter()
            .any(|x| x.ptr_eq(w))
    };
    let a2 = a.clone();
    drop(a);
    assert!(listed(&weak));
    drop(a2);
    assert_eq!(weak.strong_count(), 0);
    assert!(!listed(&weak));
    let mut got = Vec::new();
    let mut buf = [0u8; 16];
    loop {
        let n = rustix::io::read(&r1, &mut buf[..]).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"pending");
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
