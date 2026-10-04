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

/// The open list's order after closes in any order and a later opening
/// (AR-7: the list is a map keyed by the opening's serial, closing is
/// O(log N)): newest first, as the `Vec` it replaced kept it, every closed
/// file gone, the new one first. 256 handles on `/dev/null`, closed in a
/// mixed order, then the rest oldest first.
#[test]
#[cfg_attr(miri, ignore)]
fn open_list_order_after_closes() {
    let _alone = walks_alone();
    let open = || Some(Handle::open(b"/dev/null", FsMode::Write).unwrap());
    let mut hs: Vec<Option<Handle>> = (0..256).map(|_| open()).collect();
    for i in (0..256).rev().step_by(3) {
        hs[i] = None;
    }
    for i in (0..256).step_by(7) {
        hs[i] = None;
    }
    hs.push(open());
    let mine: Vec<&std::sync::Arc<_>> = hs
        .iter()
        .rev()
        .flatten()
        .map(|h| h.file_stream().unwrap())
        .collect();
    assert!(mine.len() > 100);
    let walk = crate::io::handle::open_files_newest_first();
    let order: Vec<usize> = walk
        .iter()
        .filter_map(|f| mine.iter().position(|m| std::sync::Arc::ptr_eq(m, f)))
        .collect();
    assert_eq!(order, (0..mine.len()).collect::<Vec<_>>());
    drop(walk);
    // each closes as its last handle goes, oldest first: the list's slot
    // and the handle are its only references
    for h in hs.iter_mut() {
        if let Some(x) = h.take() {
            assert_eq!(std::sync::Arc::strong_count(x.file_stream().unwrap()), 2);
        }
    }
}

/// LB-29 as the judge narrowed it: the exit's flush waits for a stream
/// whose holder is writing or busy otherwise (another thread here: the
/// drain's out-of-memory exit while `main` is in a `println`, leanrs's
/// re-review), and skips at once one whose holder is blocked reading.
#[test]
#[cfg_attr(miri, ignore)]
fn exit_lock_waits_for_writers_and_skips_readers() {
    use crate::io::exit::exit_lock;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};
    let (_r, w) = pipe();
    let file = CFile::fdopen(w, FsMode::Write);
    let busy = file.busy_arc().unwrap();
    let m = Arc::new(Mutex::new(file));
    let reading = {
        let b = busy.clone();
        move || b.load(Ordering::Acquire) == BUSY_INPUT
    };
    // the last: the holder takes the lock idle and starts reading 200 ms
    // later (the race of leanrs's re-check): seen reading, skipped
    for (what, held_ms, late) in [
        (BUSY_IDLE, 300u64, false),
        (BUSY_OUTPUT, 300, false),
        (BUSY_INPUT, 1500, false),
        (BUSY_INPUT, 3000, true),
    ] {
        let start = Arc::new(Barrier::new(2));
        let (m2, s2) = (m.clone(), start.clone());
        let holder = std::thread::spawn(move || {
            let mut g = m2.lock().unwrap();
            if what != BUSY_IDLE && !late {
                g.mark(what);
            }
            s2.wait();
            if late {
                std::thread::sleep(Duration::from_millis(200));
                g.mark(what);
            }
            std::thread::sleep(Duration::from_millis(held_ms));
            g.unmark();
        });
        start.wait();
        let t0 = Instant::now();
        let got = exit_lock(&m, &reading).is_some();
        let waited = t0.elapsed();
        if what == BUSY_INPUT {
            assert!(!got, "a reader's stream is skipped");
            assert!(waited < Duration::from_millis(1000), "{late}: {waited:?}");
        } else {
            assert!(got, "a writer's stream is waited for");
            assert!(waited >= Duration::from_millis(200), "{waited:?}");
        }
        holder.join().unwrap();
        assert_eq!(busy.load(Ordering::Acquire), BUSY_IDLE);
    }
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

/// Review RSIO-12 (the reviewer's probe): a no-suspend drop's nowait flush
/// that writes part of the pending bytes, then would block, must leave what
/// is left where the deferred close's flush writes it; before the fix `wb`
/// stayed moved, the flush sought back (`re != wb`), failed with ESPIPE on
/// the FIFO, and dropped the rest (3904 bytes). The 8192-byte buffer stands
/// for a FIFO whose `st_blksize` is 8192 or more (NFS, a 64 KiB-page
/// kernel), where FIFO writes go in PIPE_BUF pieces.
#[cfg(feature = "sched")]
#[test]
#[cfg_attr(miri, ignore)]
fn rsio12_partial_nowait_flush_then_close_keeps_every_byte() {
    use std::io::Read;
    let dir = std::env::temp_dir().join(format!("rsio12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fifo");
    assert!(std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap()
        .success());
    // the read end first (O_RDWR does not wait), then the write end
    let mut rd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let wr: OwnedFd = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .into();
    // 15 of the FIFO's 16 pages full: room for one PIPE_BUF piece
    let filler = vec![b'x'; 15 * 4096];
    let mut done = 0;
    while done < filler.len() {
        done += rustix::io::write(&wr, &filler[done..]).unwrap();
    }
    let mut f = CFile::fdopen(wr, FsMode::Write);
    f.buf = vec![0u8; 8192];
    f.has_buf = true;
    let tail = vec![b't'; 8000];
    f.put(&tail).unwrap();
    assert_eq!(f.pending_output().len(), 8000);
    let all_out = f.flush_nowait();
    eprintln!(
        "flush_nowait -> {all_out}; pending {}",
        f.pending_output().len()
    );
    assert!(!all_out, "the FIFO is full after one piece");
    // the deferred close, once the reader drains (a thread here)
    let reader = std::thread::spawn(move || {
        let mut all = vec![0u8; 15 * 4096 + 8000];
        let mut got = 0;
        // read until 200 ms pass without data
        let _ = rustix::io::ioctl_fionbio(&rd, false);
        loop {
            let mut pfd = [rustix::event::PollFd::new(
                &rd,
                rustix::event::PollFlags::IN,
            )];
            let t = rustix::event::Timespec {
                tv_sec: 0,
                tv_nsec: 200_000_000,
            };
            if rustix::event::poll(&mut pfd, Some(&t)).unwrap() == 0 {
                break;
            }
            match rd.read(&mut all[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => break,
            }
            if got == all.len() {
                break;
            }
        }
        got
    });
    f.close();
    let got = reader.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        got,
        15 * 4096 + 8000,
        "bytes lost: {}",
        15 * 4096 + 8000 - got
    );
}

/// A sink that stops once it would hold more than `cap` bytes, as a glue's
/// fallible sink whose reservation failed (lean2rr's and leanrs's); it
/// counts every byte offered to it, kept or dropped.
struct StopsAfter {
    v: Vec<u8>,
    cap: usize,
    stopped: bool,
    offered: usize,
}

impl StopsAfter {
    fn new(cap: usize) -> Self {
        StopsAfter {
            v: Vec::new(),
            cap,
            stopped: false,
            offered: 0,
        }
    }
}

impl ByteSink for StopsAfter {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.offered += bytes.len();
        if self.stopped {
            return;
        }
        if self.v.len() + bytes.len() > self.cap {
            self.stopped = true;
            self.v = Vec::new();
            return;
        }
        self.v.extend_from_slice(bytes);
    }

    fn stopped(&self) -> bool {
        self.stopped
    }
}

/// AR-19: a line that never ends (stdin from `/dev/zero`), read into a sink
/// that stops after `cap` bytes. `getLine` reads no further once the sink
/// has stopped, at most one buffer after the stop, and returns `ENOMEM`'s
/// error; before AR-19 it read and dropped zero bytes for good. The call runs
/// on a thread, so that a regression fails the test instead of hanging it.
#[test]
#[cfg_attr(miri, ignore)]
fn get_line_stops_with_its_sink() {
    const CAP: usize = 100_000;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let zero: OwnedFd = std::fs::File::open("/dev/zero").unwrap().into();
        let h = Handle::fdopen(zero, FsMode::Read);
        let mut sink = StopsAfter::new(CAP);
        let r = h.get_line(&mut sink);
        let _ = tx.send((r, sink.stopped, sink.offered, sink.v.len()));
    });
    let (r, stopped, offered, kept) = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("getLine returns once its sink has stopped");
    assert_eq!(
        r,
        Err(IoError::ResourceExhausted(
            None,
            12,
            "not enough memory".to_owned()
        ))
    );
    assert!(stopped);
    assert_eq!(kept, 0);
    assert!(
        offered > CAP && offered <= CAP + BUFSIZ,
        "{offered} bytes offered"
    );
}

/// AR-19: a sink that stops on the bytes that end the line still makes
/// `get_line` fail with `ENOMEM`, the modelled `errno` untouched; the bytes
/// it took stay consumed, and the next line reads as usual. A sink that
/// does not stop gets the line it got before.
#[test]
fn get_line_stop_on_the_line_end() {
    let (r, w) = pipe();
    rustix::io::write(&w, b"abcdef\nnext\nlast\n").unwrap();
    drop(w);
    let mut f = CFile::fdopen(r, FsMode::Read);
    set_errno(0);
    let mut sink = StopsAfter::new(3);
    assert_eq!(f.get_line(&mut sink), Err(ENOMEM));
    // `a` from the refill, then `bcdef\n` from the buffer
    assert_eq!((sink.stopped, sink.offered), (true, 7));
    assert_eq!(errno(), 0);
    let mut line = Vec::new();
    f.get_line(&mut line).unwrap();
    assert_eq!(line, b"next\n");
    let mut sink = StopsAfter::new(5);
    f.get_line(&mut sink).unwrap();
    assert_eq!((sink.stopped, &sink.v[..]), (false, &b"last\n"[..]));
}
