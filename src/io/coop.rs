//! sched-io in the io layer (features `io` and `sched`): the blocking calls
//! of the FILE model, the handles and the child processes let the
//! scheduler's other contexts run.
//!
//! Natively a read of an empty pipe, a write into a full one, `flock` and
//! `waitpid` block only the calling thread, while the program's other
//! threads go on. Here every context runs on one thread, so in a program
//! with other contexts ([`sched::io_cooperative`]) each such call first
//! waits for its descriptor with the scheduler's event loop, and the others
//! run meanwhile:
//! - **reads** (`read(2)` of a pipe, a FIFO, a socket, a terminal, a child's
//!   standard output) wait until the descriptor is readable, then make the
//!   same `read(2)`, which no longer blocks: the same bytes and errors. A
//!   read of the controlling terminal from a background process group is
//!   plain, so it gets `SIGTTIN` at once, as natively (review RSIO-08);
//! - **writes** to pipes and sockets try `pwritev2(RWF_NOWAIT)` (it writes
//!   what fits, as `write(2)` on a non-blocking descriptor), and on `EAGAIN`
//!   wait until the descriptor is writable; to a FIFO (no `RWF_NOWAIT`:
//!   `EOPNOTSUPP`) they wait until it is writable and write at most
//!   `PIPE_BUF` bytes, which a writable FIFO always takes (a write of more
//!   is not atomic natively either); to a terminal or another character
//!   device they wait until it is writable, then make one `write(2)` of the
//!   whole rest, atomic as natively (review RSIO-05);
//! - **`Handle.lock`** retries `flock` with `LOCK_NB`: woken by an unlock in
//!   this process, and every few milliseconds for another process's;
//! - **`Child.wait`** waits until the child's pidfd is readable (it has
//!   exited), then makes the same `waitpid`.
//!
//! Regular files, block devices and directories never block, so their calls
//! stay plain, as natively; so do descriptors already in non-blocking mode
//! (whose `EAGAIN` is the result). A program that has created no task,
//! promise, timer or watch never gets here (`sched::coop_possible`, one
//! relaxed load), and neither does a call made with no other context about
//! (the speed floor O12), nor one in a no-suspend scope
//! (`sched::enter_no_suspend`: the glue's free and drop paths).
//!
//! **Stream locks.** A stream's `FILE` lock (a `std::sync::Mutex`) stays
//! held across such a wait, as glibc's lock stays held while its thread
//! blocks in `read(2)`. Another context that wants the stream then must wait
//! for it *cooperatively*: a plain `Mutex::lock` from the same thread would
//! deadlock. So once `coop_possible`, every stream lock is taken through
//! [`lock`], which records it in the running context's `HELD` list. Every
//! context switch (`sched`'s `switch_away`, whatever the reason: an IO
//! wait, a promise, a sleep, a yield point) moves the suspending context's
//! `HELD` locks to `OWNED`, under its id, and gives them back when it goes
//! on ([`park`], review RSIO-01). A context that finds a stream locked by a
//! suspended context of this thread (`OWNED`) waits for it
//! ([`crate::sched::block_sync`]) and is woken when the guard drops. A
//! stream held by another OS thread is waited for with the plain
//! `Mutex::lock`, as before.

use super::cfile::CFile;
use super::error::set_errno;
use super::handle::StreamGuard;
use super::sys::Fd;
use crate::sched::{self, CtxId, Interest, PollItem};
use rustix::fd::{AsFd, BorrowedFd};
use rustix::fs::FlockOperation;
use rustix::io::Errno;
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

/// POSIX's `PIPE_BUF`: a writable pipe takes this many bytes without
/// blocking.
const PIPE_BUF: usize = 4096;

/// What a stream's descriptor is for the cooperative path, found the first
/// time a cooperative call needs it (`fstat` and `F_GETFL`, without touching
/// the modelled `errno`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coop(u8);

impl Coop {
    /// Not looked at yet.
    pub(crate) const UNKNOWN: Coop = Coop(0);
    /// Calls stay plain: a regular file, a block device, a directory, a
    /// descriptor in non-blocking mode, or none.
    const PLAIN: Coop = Coop(1);
    /// A pipe or a socket: wait first; writes try `RWF_NOWAIT`.
    const WAIT: Coop = Coop(2);
    /// A pipe without `RWF_NOWAIT` (a FIFO): writes of at most `PIPE_BUF`.
    const WAIT_NO_NOWAIT: Coop = Coop(3);
    /// A character device (a terminal): writes of the whole rest, once
    /// writable; reads of the controlling terminal from a background group
    /// are plain.
    const CHAR: Coop = Coop(4);

    fn of(fd: &Fd) -> Coop {
        let Some(b) = fd.borrow() else {
            return Coop::PLAIN;
        };
        let Ok(st) = rustix::fs::fstat(b) else {
            return Coop::PLAIN;
        };
        let kind = match st.st_mode & 0o170000 {
            // regular, block device, directory
            0o100000 | 0o060000 | 0o040000 => return Coop::PLAIN,
            0o020000 => Coop::CHAR,
            _ => Coop::WAIT,
        };
        match rustix::fs::fcntl_getfl(b) {
            Ok(fl) if !fl.contains(rustix::fs::OFlags::NONBLOCK) => kind,
            _ => Coop::PLAIN,
        }
    }

    /// Whether a call on `fd` cooperates now (classifying it the first
    /// time). A descriptor known to stay plain costs no scheduler check
    /// (review RSIO-04).
    fn waits(&mut self, fd: &Fd) -> bool {
        if *self == Coop::PLAIN || !sched::io_cooperative() {
            return false;
        }
        if *self == Coop::UNKNOWN {
            *self = Coop::of(fd);
        }
        *self != Coop::PLAIN
    }
}

/// Whether `fd` is the controlling terminal of this process and its
/// foreground process group is not this process's: a read then gets
/// `SIGTTIN` (or `EIO`) at once natively, so it is made plain.
fn background_tty(fd: BorrowedFd<'_>) -> bool {
    match rustix::termios::tcgetpgrp(fd) {
        Ok(pg) => pg != rustix::process::getpgrp(),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Stream locks

/// A stream lock held by a context that suspended inside the io layer.
struct Owned {
    /// The stream's `Mutex` (its address).
    key: usize,
    owner: CtxId,
    /// Contexts waiting for the stream.
    waiters: Vec<CtxId>,
}

thread_local! {
    /// The stream locks the running context holds (taken through `lock`),
    /// innermost last. Empty whenever a context switch happens outside the
    /// io layer.
    static HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// The stream locks of contexts that suspended inside the io layer while
    /// holding them, until each is released.
    static OWNED: RefCell<Vec<Owned>> = const { RefCell::new(Vec::new()) };
    /// Contexts waiting in `flock`.
    static FLOCK_WAITERS: RefCell<Vec<CtxId>> = const { RefCell::new(Vec::new()) };
}

fn key_of(m: &Mutex<CFile>) -> usize {
    m as *const Mutex<CFile> as usize
}

fn held(g: std::sync::MutexGuard<'_, CFile>, key: usize) -> StreamGuard<'_> {
    // While the thread's locals are being destroyed (a lock taken from a
    // thread-local's destructor), the lock is a plain one (review RSIO-07).
    match HELD.try_with(|h| h.borrow_mut().push(key)) {
        Ok(()) => StreamGuard::tracked(g, key),
        Err(_) => StreamGuard::plain(g),
    }
}

/// A stream's lock, taken cooperatively (see the module comment).
pub(crate) fn lock(m: &Mutex<CFile>) -> StreamGuard<'_> {
    let key = key_of(m);
    loop {
        match m.try_lock() {
            Ok(g) => return held(g, key),
            Err(TryLockError::Poisoned(p)) => return held(p.into_inner(), key),
            Err(TryLockError::WouldBlock) => {
                if !wait_for_owner(key) {
                    // another thread holds it: it will let go
                    return held(m.lock().unwrap_or_else(PoisonError::into_inner), key);
                }
            }
        }
    }
}

/// A stream's lock if it is free (glibc's `_IO_unbuffer_all` skips a
/// stream another thread holds).
pub(crate) fn try_lock(m: &Mutex<CFile>) -> Option<StreamGuard<'_>> {
    let key = key_of(m);
    match m.try_lock() {
        Ok(g) => Some(held(g, key)),
        Err(TryLockError::Poisoned(p)) => Some(held(p.into_inner(), key)),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// The stream `key` is locked: if a suspended context of this thread holds
/// it, wait until it lets go (true); otherwise false (another thread holds
/// it). In a no-suspend scope the wait is impossible, and the plain lock
/// would never return: that is a panic, with the reason.
fn wait_for_owner(key: usize) -> bool {
    if !crate::sched::alive() {
        return false;
    }
    let me = sched::current_context();
    let theirs = OWNED
        .try_with(|o| {
            let mut o = o.borrow_mut();
            match o.iter_mut().find(|e| e.key == key && e.owner != me) {
                Some(e) if !sched::in_no_suspend() => {
                    e.waiters.push(me);
                    Some(true)
                }
                Some(_) => None,
                None => Some(false),
            }
        })
        .unwrap_or(Some(false));
    match theirs {
        Some(true) => {
            sched::block_sync();
            true
        }
        Some(false) => false,
        None => panic!(
            "lean-runtime: a stream that a suspended task holds is needed in a no-suspend scope"
        ),
    }
}

/// A guard taken through `lock` is dropped: the running context no longer
/// holds the stream, and whoever waits for it may go on.
pub(crate) fn released(key: usize) {
    let _ = HELD.try_with(|h| {
        let mut h = h.borrow_mut();
        if let Some(p) = h.iter().rposition(|&k| k == key) {
            h.remove(p);
        }
    });
    let waiters = OWNED
        .try_with(|o| {
            let mut o = o.borrow_mut();
            match o.iter().position(|e| e.key == key) {
                Some(p) => o.swap_remove(p).waiters,
                None => Vec::new(),
            }
        })
        .unwrap_or_default();
    if !waiters.is_empty() && crate::sched::alive() {
        for c in waiters {
            sched::wake(c);
        }
    }
}

/// The stream locks context `cur` holds, recorded as held by a suspended
/// context while it is switched away; dropping the value gives them back
/// to it. Called by the scheduler at every switch (`switch_away`).
pub(crate) fn park(cur: CtxId) -> Parked {
    let held = HELD
        .try_with(|h| std::mem::take(&mut *h.borrow_mut()))
        .unwrap_or_default();
    if !held.is_empty() {
        let _ = OWNED.try_with(|o| {
            let mut o = o.borrow_mut();
            for &k in &held {
                if !o.iter().any(|e| e.key == k) {
                    o.push(Owned {
                        key: k,
                        owner: cur,
                        waiters: Vec::new(),
                    });
                }
            }
        });
    }
    Parked(held)
}

/// A suspended context's stream locks ([`park`]), given back on drop: when
/// the context goes on, or when a panic unwinds its switch.
pub(crate) struct Parked(Vec<usize>);

impl Drop for Parked {
    fn drop(&mut self) {
        let held = std::mem::take(&mut self.0);
        let _ = HELD.try_with(|h| {
            let mut h = h.borrow_mut();
            debug_assert!(
                h.is_empty(),
                "lean-runtime: stream locks left by another context"
            );
            h.extend(held);
        });
    }
}

// ---------------------------------------------------------------------------
// Reads and writes

/// Wait until `fd` is ready for `interest`, letting the other contexts run.
fn wait(fd: BorrowedFd<'_>, interest: Interest) {
    let _ = sched::wait_fd(fd, interest);
}

/// Before a `read(2)` of the stream's descriptor: in a program with other
/// contexts, wait until it is readable (but for the controlling terminal
/// read from a background process group: `background_tty`).
pub(crate) fn before_read(fd: &Fd, coop: &mut Coop) {
    if coop.waits(fd) {
        if let Some(b) = fd.borrow() {
            if *coop == Coop::CHAR && background_tty(b) {
                return;
            }
            wait(b, Interest::READ);
        }
    }
}

/// One `write(2)` of `data` on the stream's descriptor, cooperatively: the
/// count written, or the error (recorded in the modelled `errno`); `None`
/// when the call is to stay plain.
pub(crate) fn write_once(fd: &Fd, data: &[u8], coop: &mut Coop) -> Option<Result<usize, i32>> {
    if !coop.waits(fd) {
        return None;
    }
    let b = fd.borrow()?;
    loop {
        if *coop == Coop::WAIT {
            let slice = [std::io::IoSlice::new(data)];
            match rustix::io::pwritev2(b, &slice, u64::MAX, rustix::io::ReadWriteFlags::NOWAIT) {
                Ok(n) => return Some(Ok(n)),
                Err(Errno::AGAIN) => {
                    wait(b, Interest::WRITE);
                    continue;
                }
                Err(Errno::OPNOTSUPP) | Err(Errno::NOSYS) => *coop = Coop::WAIT_NO_NOWAIT,
                Err(e) => {
                    let code = e.raw_os_error();
                    set_errno(code);
                    return Some(Err(code));
                }
            }
        }
        wait(b, Interest::WRITE);
        if *coop == Coop::CHAR {
            // A terminal: one write(2) of the whole rest, atomic against
            // other writers as natively (the tty layer's write lock). It is
            // writable with 256 bytes free (`WAKEUP_CHARS`), so the write may
            // still block the thread until the terminal drains, as natively
            // the writing thread blocks.
            return Some(fd.write(data));
        }
        // A writable FIFO takes `PIPE_BUF` bytes.
        return Some(fd.write(&data[..data.len().min(PIPE_BUF)]));
    }
}

/// One `write(2)` of `data` that never blocks, for a drop's flush in a
/// no-suspend scope (`CFile::flush_nowait`, review RSIO-09): `None` for a
/// descriptor that never blocks (its usual write is fine), the count
/// written, or the error, `EAGAIN` when the descriptor would block. Pipes
/// and sockets: `pwritev2(RWF_NOWAIT)`; a FIFO: `PIPE_BUF` bytes once
/// `poll(2)` says it is writable; a terminal: the whole rest then, as its
/// cooperative write does.
pub(crate) fn write_nowait(fd: &Fd, data: &[u8], coop: &mut Coop) -> Option<Result<usize, i32>> {
    if *coop == Coop::UNKNOWN {
        *coop = Coop::of(fd);
    }
    if *coop == Coop::PLAIN {
        return None;
    }
    let b = fd.borrow()?;
    let fail = |e: Errno| {
        let code = e.raw_os_error();
        set_errno(code);
        Some(Err(code))
    };
    if *coop == Coop::WAIT {
        let slice = [std::io::IoSlice::new(data)];
        match rustix::io::pwritev2(b, &slice, u64::MAX, rustix::io::ReadWriteFlags::NOWAIT) {
            Ok(n) => return Some(Ok(n)),
            // would block: not an error of the flush, so the modelled errno
            // stays as native's blocking `fclose` leaves it (review RSIO-13)
            Err(Errno::AGAIN) => return Some(Err(Errno::AGAIN.raw_os_error())),
            Err(Errno::OPNOTSUPP) | Err(Errno::NOSYS) => *coop = Coop::WAIT_NO_NOWAIT,
            Err(e) => return fail(e),
        }
    }
    let mut fds = [rustix::event::PollFd::from_borrowed_fd(
        b,
        rustix::event::PollFlags::OUT,
    )];
    let zero = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut fds, Some(&zero)) {
        Ok(0) => return Some(Err(Errno::AGAIN.raw_os_error())),
        Ok(_) => {}
        Err(e) => return fail(e),
    }
    let n = if *coop == Coop::CHAR {
        data.len()
    } else {
        data.len().min(PIPE_BUF)
    };
    Some(fd.write(&data[..n]))
}

thread_local! {
    /// Streams whose drop in a no-suspend scope found their pipe full: their
    /// flush and close wait for the end of the outermost scope.
    static DEFERRED: RefCell<Vec<CFile>> = const { RefCell::new(Vec::new()) };
}

/// How many streams wait in `DEFERRED` (any thread): one relaxed load at
/// each outermost `leave_no_suspend`.
static DEFERRED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Set a stream aside, its flush and close to happen when this thread's
/// outermost no-suspend scope ends (`close_deferred`), or at the exit's
/// flush (`exit::exit_flush`).
pub(crate) fn defer_close(f: CFile) {
    if DEFERRED.try_with(|d| d.borrow_mut().push(f)).is_ok() {
        DEFERRED_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Whether a stream waits to be closed (`defer_close`).
#[inline]
pub(crate) fn deferred_pending() -> bool {
    DEFERRED_COUNT.load(Ordering::Relaxed) > 0
}

/// Close this thread's deferred streams, oldest first: each flush may wait
/// cooperatively now (`leave_no_suspend` at the outermost scope's end, and
/// the exit's flush).
pub(crate) fn close_deferred() {
    loop {
        let next = DEFERRED
            .try_with(|d| {
                let mut d = d.borrow_mut();
                (!d.is_empty()).then(|| d.remove(0))
            })
            .ok()
            .flatten();
        let Some(mut f) = next else { return };
        DEFERRED_COUNT.fetch_sub(1, Ordering::Relaxed);
        f.close();
        drop(f);
        flock_released();
    }
}

/// Before a blocking read of any of `fds` (`IO.Process.output`'s pipes):
/// in a program with other contexts, wait until one is readable.
/// The caller has checked `sched::io_cooperative()` (review RSIO-04).
pub(crate) fn before_read_any(fds: &[BorrowedFd<'_>]) {
    let mut items: Vec<PollItem<'_>> = fds
        .iter()
        .map(|&f| PollItem::new(f, Interest::READ))
        .collect();
    let _ = sched::poll_fds(&mut items, None);
}

// ---------------------------------------------------------------------------
// flock

/// How long a context waiting in `flock` waits before it tries again (the
/// lock may be released by another process, which nothing here sees);
/// doubled from `FLOCK_FIRST` up to `FLOCK_MAX`. An unlock in this process
/// wakes it at once.
const FLOCK_FIRST: Duration = Duration::from_millis(1);
const FLOCK_MAX: Duration = Duration::from_millis(16);

/// `flock(fd, op)` for a blocking `op`, cooperatively: `None` when the call
/// is to stay plain (no other context, or a non-blocking `op`).
pub(crate) fn flock(fd: &Fd, op: FlockOperation) -> Option<Result<(), i32>> {
    let nb = match op {
        FlockOperation::LockShared => FlockOperation::NonBlockingLockShared,
        FlockOperation::LockExclusive => FlockOperation::NonBlockingLockExclusive,
        _ => return None,
    };
    if !sched::io_cooperative() {
        return None;
    }
    let b = fd.borrow()?;
    let mut nap = FLOCK_FIRST;
    loop {
        match rustix::fs::flock(b, nb) {
            Ok(()) => return Some(Ok(())),
            Err(Errno::WOULDBLOCK) => {
                let me = sched::current_context();
                FLOCK_WAITERS.with(|w| w.borrow_mut().push(me));
                sched::block_until(Instant::now() + nap);
                FLOCK_WAITERS.with(|w| w.borrow_mut().retain(|&c| c != me));
                nap = (nap * 2).min(FLOCK_MAX);
            }
            Err(e) => {
                let code = e.raw_os_error();
                set_errno(code);
                return Some(Err(code));
            }
        }
    }
}

/// A `flock` lock of this process was released (an unlock, a closed file):
/// the contexts waiting in `flock` try again.
pub(crate) fn flock_released() {
    let w = FLOCK_WAITERS
        .try_with(|w| std::mem::take(&mut *w.borrow_mut()))
        .unwrap_or_default();
    if !w.is_empty() && crate::sched::alive() {
        for c in w {
            sched::wake(c);
        }
    }
}

// ---------------------------------------------------------------------------
// Child processes

/// Before `waitpid(pid, &status, 0)` (`Child.wait`): in a program with other
/// contexts, wait until the child has exited, through its pidfd (readable
/// once it has exited), or, where `pidfd_open` fails, by looking again every
/// few milliseconds. `waitid(WNOWAIT)` looks without reaping, so the
/// `waitpid` that follows gives the status, or `ECHILD` for a pid that is
/// not a child, as without the wait.
pub(crate) fn before_waitpid(pid: rustix::process::Pid) {
    use rustix::process::{waitid, WaitId, WaitIdOptions};
    if !sched::io_cooperative() {
        return;
    }
    let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty());
    let mut nap = FLOCK_FIRST;
    loop {
        let peek = waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        );
        if !matches!(peek, Ok(None)) {
            // it has exited, or it is no child of this process
            return;
        }
        match &pidfd {
            Ok(fd) => wait(fd.as_fd(), Interest::READ),
            Err(_) => {
                sched::block_until(Instant::now() + nap);
                nap = (nap * 2).min(FLOCK_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::handle::{lock, STDOUT};

    #[test]
    fn stream_locks_are_tracked_once_tasks_may_exist() {
        crate::sched::reactor_coop_on_for_tests();
        let key = key_of(&STDOUT);
        {
            let _g = lock(&STDOUT);
            assert_eq!(HELD.with(|h| h.borrow().clone()), [key]);
            // a second stream inside the first, as `underflow` takes stdout
            let m = Mutex::new(CFile::std(2));
            let _g2 = lock(&m);
            assert_eq!(HELD.with(|h| h.borrow().len()), 2);
        }
        assert!(HELD.with(|h| h.borrow().is_empty()));
        assert!(OWNED.with(|o| o.borrow().is_empty()));
    }

    /// Every switch records the running context's stream locks as held by a
    /// suspended context, and gives them back when it goes on (RSIO-01).
    #[test]
    fn a_switch_parks_the_stream_locks() {
        crate::sched::reactor_coop_on_for_tests();
        let key = key_of(&STDOUT);
        let g = lock(&STDOUT);
        let parked = park(crate::sched::MAIN);
        assert!(HELD.with(|h| h.borrow().is_empty()));
        assert_eq!(
            OWNED.with(|o| o.borrow().iter().map(|e| e.key).collect::<Vec<_>>()),
            [key]
        );
        drop(parked);
        assert_eq!(HELD.with(|h| h.borrow().clone()), [key]);
        drop(g);
        assert!(HELD.with(|h| h.borrow().is_empty()));
        assert!(OWNED.with(|o| o.borrow().is_empty()));
    }

    /// RSIO-09: a full pipe keeps the pending bytes for a later flush; a
    /// drained one takes them, without blocking either way.
    #[test]
    fn a_nowait_flush_keeps_what_a_full_pipe_refuses() {
        use crate::io::handle::FsMode;
        let (r, w) = rustix::pipe::pipe().unwrap();
        let cap = rustix::pipe::fcntl_getpipe_size(&w).unwrap();
        let mut f = CFile::fdopen(w, FsMode::Write);
        f.put(b"tail").unwrap();
        // the pipe full
        let filler = vec![b'x'; cap];
        let mut done = 0;
        while done < cap {
            done += rustix::io::write(f.descriptor().borrow().unwrap(), &filler[done..]).unwrap();
        }
        assert!(!f.flush_nowait());
        assert_eq!(f.pending_output(), b"tail");
        let mut buf = vec![0u8; cap];
        let mut got = 0;
        while got < cap {
            got += rustix::io::read(&r, &mut buf[got..]).unwrap();
        }
        assert!(f.flush_nowait());
        assert!(f.pending_output().is_empty());
        let mut t = [0u8; 8];
        assert_eq!(rustix::io::read(&r, &mut t).unwrap(), 4);
        assert_eq!(&t[..4], b"tail");
    }

    #[test]
    fn a_no_suspend_scope_makes_io_plain() {
        assert!(!sched::in_no_suspend());
        let a = sched::no_suspend();
        sched::enter_no_suspend();
        assert!(sched::in_no_suspend());
        assert!(!sched::io_cooperative());
        sched::leave_no_suspend();
        assert!(sched::in_no_suspend());
        drop(a);
        assert!(!sched::in_no_suspend());
    }

    #[test]
    fn what_cooperates() {
        let (r, w) = rustix::pipe::pipe().unwrap();
        assert_eq!(
            Coop::of(&Fd::Owned(std::sync::Arc::new(r.into()))),
            Coop::WAIT
        );
        rustix::fs::fcntl_setfl(&w, rustix::fs::OFlags::NONBLOCK).unwrap();
        // already non-blocking: its `EAGAIN` is the result
        assert_eq!(
            Coop::of(&Fd::Owned(std::sync::Arc::new(w.into()))),
            Coop::PLAIN
        );
        let f = std::fs::File::open("/proc/self/exe").unwrap();
        assert_eq!(Coop::of(&Fd::Owned(std::sync::Arc::new(f))), Coop::PLAIN);
        // a character device: whole writes once writable (RSIO-05)
        let n = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .unwrap();
        assert_eq!(Coop::of(&Fd::Owned(std::sync::Arc::new(n))), Coop::CHAR);
        assert_eq!(Coop::of(&Fd::Closed), Coop::PLAIN);
    }
}
