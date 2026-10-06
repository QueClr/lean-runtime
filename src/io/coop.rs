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
//! stay plain, as natively; so do descriptors in non-blocking mode (whose
//! `EAGAIN` is the result), a mode read at every call: it belongs to the
//! open file description, which another process may change at any time
//! ([`nonblocking`]). A program that has created no task,
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
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

/// POSIX's `PIPE_BUF`: a writable pipe takes this many bytes without
/// blocking.
const PIPE_BUF: usize = 4096;

/// What a stream's descriptor is for the cooperative path, found the first
/// time a cooperative call needs it (`fstat`, without touching the modelled
/// `errno`). Its non-blocking mode is not part of it: that is read at every
/// call ([`nonblocking`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coop(u8);

impl Coop {
    /// Not looked at yet.
    pub(crate) const UNKNOWN: Coop = Coop(0);
    /// Calls stay plain: a regular file, a block device, a directory, or
    /// none.
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
        match st.st_mode & 0o170000 {
            // regular, block device, directory
            0o100000 | 0o060000 | 0o040000 => Coop::PLAIN,
            0o020000 => Coop::CHAR,
            _ => Coop::WAIT,
        }
    }

    /// Whether a call on `fd` cooperates now (classifying it the first
    /// time): not while the descriptor is in non-blocking mode. A
    /// descriptor known to stay plain costs no scheduler check (review
    /// RSIO-04).
    fn waits(&mut self, fd: &Fd) -> bool {
        if *self == Coop::PLAIN || !sched::io_cooperative() {
            return false;
        }
        if *self == Coop::UNKNOWN {
            *self = Coop::of(fd);
        }
        *self != Coop::PLAIN && !nonblocking(fd)
    }
}

/// Whether `fd` is in non-blocking mode now (`F_GETFL`; a failure counts as
/// non-blocking: the call stays plain), so its `EAGAIN` is the result, as
/// natively. Read at every cooperative call, one system call: the flag
/// belongs to the open file description, which the processes that share it
/// (the parent, a child spawned with `inherit`) may change at any time
/// (HIO-06 of the io bug hunt). Read once, a descriptor made blocking later
/// would get a plain read or write that blocks the scheduler's only thread,
/// so a task that would make it ready (a task that writes a prompt the peer
/// waits for before it sends the input) never runs and the program waits
/// for good; one made non-blocking later would wait where native's call
/// returns `EAGAIN`.
fn nonblocking(fd: &Fd) -> bool {
    match fd.borrow().map(rustix::fs::fcntl_getfl) {
        Some(Ok(fl)) => fl.contains(rustix::fs::OFlags::NONBLOCK),
        _ => true,
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
    /// The waiters an unlock in this process found able to run (woken from
    /// their nap, or already runnable): their wait ended with the release
    /// (`flock_last_wake`).
    static FLOCK_HANDED: RefCell<Vec<CtxId>> = const { RefCell::new(Vec::new()) };
    /// How this thread's last cooperative `flock` ended (`flock_last_wake`).
    static FLOCK_LAST: Cell<FlockWake> = const { Cell::new(FlockWake::AtOnce) };
}

/// How a cooperative `flock` that got its lock ended its last wait: a test
/// hook (`flock_last_wake`), so that a regression of the handoff (the
/// waiter left napping after an unlock in this process, up to `FLOCK_MAX`;
/// review NEW-1 of wait-1) shows without a clock.
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlockWake {
    /// It got the lock without a wait.
    AtOnce,
    /// Its last wait ended with an unlock in this process, which found it
    /// able to run (`flock_released`).
    Unlock,
    /// Its last wait ended when its nap ran out.
    NapEnd,
}

/// How this thread's last cooperative `flock` that got its lock ended its
/// last wait (a test hook of the crate's drivers).
#[doc(hidden)]
pub fn flock_last_wake() -> FlockWake {
    FLOCK_LAST.with(Cell::get)
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

/// A stream's lock, taken cooperatively (see the module comment). First,
/// with no stream lock held yet, the context's handed-off streams end
/// ([`join_own_writers`]): natively its thread was in their `fclose` until
/// then, so a write through another descriptor of the same pipe comes
/// after their bytes (leanrs's FIFO probe, case `process/handoff_then_write`).
pub(crate) fn lock(m: &Mutex<CFile>) -> StreamGuard<'_> {
    join_own_writers(JoinAt::End);
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

/// Who holds stream `m`, as far as this thread knows (the exit's
/// `exit_lock`): the running context itself (`HELD`), a suspended context
/// of this thread (`OWNED`), or neither (another thread, or nobody).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holder {
    Me,
    Suspended,
    Other,
}

/// See [`Holder`].
pub(crate) fn holder_of(m: &Mutex<CFile>) -> Holder {
    let key = key_of(m);
    if HELD
        .try_with(|h| h.borrow().contains(&key))
        .unwrap_or(false)
    {
        return Holder::Me;
    }
    if OWNED
        .try_with(|o| o.borrow().iter().any(|e| e.key == key))
        .unwrap_or(false)
    {
        return Holder::Suspended;
    }
    Holder::Other
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
    if *coop == Coop::PLAIN || nonblocking(fd) {
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

// ---------------------------------------------------------------------------
// The hand-off of a dropped stream's last bytes (review RSIO-09; AR-8)

/// A writer thread ([`hand_off`]) that has not ended, and the context that
/// dropped the stream: natively that context's thread would still be inside
/// `fclose` until the writes end. The thread removes its own entry when it
/// has written and closed, so nothing of it stays behind (review RFX1-19).
struct Writer {
    id: u64,
    owner: Owner,
}

/// A context of a thread: the thread's tag (`thread_tag`) and its context
/// (`None` where the scheduler's state cannot be read).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Owner(u64, Option<CtxId>);

/// The writer threads that still run, with their dropping contexts.
static WRITERS: Mutex<Vec<Writer>> = Mutex::new(Vec::new());

/// Notified when a writer has removed its entry: the plain waits of
/// [`join_own_writers`] (off the scheduler, or with no other context).
static WRITER_ENDED: std::sync::Condvar = std::sync::Condvar::new();

/// How many writers `WRITERS` holds, kept under its lock: the one relaxed
/// load at every writers point ([`join_own_writers`]), so only running
/// writers cost anything (review RFX1-16).
static RUNNING_WRITERS: AtomicUsize = AtomicUsize::new(0);

/// The stack of a writer thread: a write loop needs little (the system's
/// minimum applies when it is larger; review RFX1-11).
const WRITER_STACK: usize = 64 * 1024;

/// A number naming the calling thread for `Owner` (0 while its locals are
/// being destroyed).
fn thread_tag() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static TAG: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    TAG.try_with(|t| *t).unwrap_or(0)
}

/// The running context of the calling thread.
fn me() -> Owner {
    Owner(thread_tag(), sched::running_context())
}

#[cfg(test)]
thread_local! {
    /// Test hook: this thread's hand-offs find no thread to start (`EAGAIN`).
    pub(crate) static FAIL_WRITER_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The blocking writes of `bytes` to `fd`, as glibc's `new_do_write` makes
/// them in `fclose`'s flush (until every byte is written or a write fails:
/// `EPIPE` when the reader has gone, since the process ignores `SIGPIPE`),
/// then the close (the last clone of the descriptor goes). A failure's
/// `errno` is this thread's own (review RFX1-06).
fn write_then_close(bytes: &[u8], fd: Fd) {
    let mut done = 0;
    while done < bytes.len() {
        match fd.write(&bytes[done..]) {
            Ok(n) => done += n,
            Err(_) => break,
        }
    }
    drop(fd);
}

/// A dropped stream whose pending bytes its descriptor would not take
/// without blocking (`CFile::flush_nowait` in a no-suspend scope, review
/// RSIO-09): the bytes and the descriptor go to a writer thread of their
/// own, which writes them with blocking writes and closes the descriptor,
/// as natively the drop's `fclose` does; the drop returns at once, with no
/// suspension, no lock to wait for and no scheduling point needed (AR-8;
/// reviews RFX1-01 to RFX1-03). The thread holds plain data only (a
/// `Vec<u8>` and the descriptor), never a Lean value: an internal helper,
/// not parallelism a Lean program can see. One thread per hand-off,
/// with a small stack, so a pipe that never drains blocks no other stream's
/// close.
///
/// The writer belongs to the dropping context, whose thread natively would
/// still be in `fclose`: that context waits for it at its next point where
/// it publishes or may suspend, and at its exit ([`join_own_writers`]); the
/// exit of another context does not (review RFX1-09: natively glibc unlinks
/// a stream before `fclose` flushes it, so `exit` never waits for another
/// thread's `fclose` in progress).
///
/// Where no thread can start (`EAGAIN`: the thread limit, no memory for a
/// stack), the dropping thread writes and closes itself, blocking as a
/// plain `fclose` does: no panic, no byte lost. Its limit: if the pipe's
/// reader is a task of this program, the blocked thread cannot run it, and
/// the program waits for good (review RFX1-11).
pub(crate) fn hand_off(bytes: Vec<u8>, fd: Fd) {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    {
        // registered before the thread starts, so its removal comes after
        let mut w = WRITERS.lock().unwrap_or_else(PoisonError::into_inner);
        w.push(Writer { id, owner: me() });
        RUNNING_WRITERS.store(w.len(), Ordering::Relaxed);
    }
    let job = std::sync::Arc::new(Mutex::new(Some((bytes, fd))));
    let theirs = job.clone();
    let run = move || {
        /// The entry goes however the thread ends, a panic included, so its
        /// owner never waits for good (review RFX1-22).
        struct Ended(u64);
        impl Drop for Ended {
            fn drop(&mut self) {
                writer_ended(self.0);
            }
        }
        let _ended = Ended(id);
        let taken = theirs.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some((b, fd)) = taken {
            write_then_close(&b, fd);
        }
    };
    #[cfg(test)]
    let refused = FAIL_WRITER_SPAWN.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let refused = false;
    let spawned = if refused {
        drop(run);
        false
    } else {
        // detached: the thread removes its own entry (`writer_ended`)
        std::thread::Builder::new()
            .name("lean-runtime-close".to_owned())
            .stack_size(WRITER_STACK)
            .spawn(run)
            .is_ok()
    };
    if !spawned {
        writer_ended(id);
        let taken = job.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some((b, fd)) = taken {
            write_then_close(&b, fd);
        }
    }
}

/// Writer `id` has written and closed (or never started): its entry goes,
/// and the plain waits look again.
fn writer_ended(id: u64) {
    let mut w = WRITERS.lock().unwrap_or_else(PoisonError::into_inner);
    w.retain(|w| w.id != id);
    RUNNING_WRITERS.store(w.len(), Ordering::Relaxed);
    drop(w);
    WRITER_ENDED.notify_all();
}

/// When the calling context waits for its writers ([`join_own_writers`]).
/// A byte, as the argument of an `extern "C"` function.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum JoinAt {
    /// A point where the context publishes or may suspend (`sched`'s
    /// `writers_point`: effect points, polls, sleeps, waits, promise
    /// resolutions, task creations, `Std.Sync` operations, the glue's
    /// reference writes, the end of a task's job or of `main`; and the entry
    /// of every stream lock, `flock` and `Child.wait`). Skipped in a
    /// no-suspend scope, while the context holds a stream lock (the wait
    /// could need that stream: review RFX1-02's shape) and while a panic
    /// unwinds; the next such point, or the exit, waits instead. The writer
    /// is a thread, so the pipe drains without the context, and skipping
    /// never hangs (RFX1-03's shape).
    End,
    /// The exit (`IO.Process.exit`, an internal panic, `forceExit`).
    Exit,
}

/// Wait until the writer threads of the calling context ([`hand_off`]) have
/// written and closed their streams, as natively its thread was inside
/// `fclose` until then, so that nothing the context does after the drop is
/// seen first (review RFX1-07; leanrs's re-checks of a771e57 and 78edede,
/// reviews RFX1-14 and RFX1-18: a promise it resolves, a lock it releases,
/// a reference it writes, a write through another descriptor of the same
/// pipe, a child it kills, a directory it creates). While the scheduler runs
/// other contexts (`sched::io_cooperative`), the context waits in the
/// scheduler, looking again every 1 to 16 ms (the event loop's timers wake
/// it), and the other contexts run meanwhile (a task of this program may be
/// the pipe's reader), as natively the program's other threads ran while
/// the drop's `fclose` waited. It takes no descriptor (review RFX1-19).
/// Otherwise (no other context, off the scheduler's thread, an exit in a
/// no-suspend scope) the thread waits on a condition variable the writers
/// notify. Other contexts' writers are not waited for. One relaxed load
/// when no writer runs: inlined into every caller, the rest out of line
/// (review AR-23: a translator's crate built without LTO calls nothing
/// on its hot path).
#[inline]
pub(crate) fn join_own_writers(at: JoinAt) {
    if RUNNING_WRITERS.load(Ordering::Relaxed) != 0 {
        join_own_writers_slow(at);
    }
}

/// [`join_own_writers`] once a writer runs. `extern "C"`, so it cannot
/// unwind (review AR-28): a caller that holds values with destructors
/// across the inlined fast path then needs no cleanup path for this call,
/// which would make it too costly for LLVM to inline into the translator's
/// code. A Rust panic in it, or one that a context's panic resumes here
/// while it waits, aborts the process, as at an FFI boundary (both
/// translators call it from FFI code or with `panic = "abort"` anyway;
/// docs/sched.md, "Costs to measure").
#[cold]
#[inline(never)]
extern "C" fn join_own_writers_slow(at: JoinAt) {
    if at == JoinAt::End
        && (sched::in_no_suspend()
            || std::thread::panicking()
            || HELD.try_with(|h| !h.borrow().is_empty()).unwrap_or(true))
    {
        return;
    }
    let me = me();
    let mut nap = FLOCK_FIRST;
    loop {
        let w = WRITERS.lock().unwrap_or_else(PoisonError::into_inner);
        if !w.iter().any(|w| w.owner == me) {
            return;
        }
        if sched::io_cooperative() {
            drop(w);
            sched::block_until(Instant::now() + nap);
            nap = (nap * 2).min(FLOCK_MAX);
        } else {
            drop(WRITER_ENDED.wait(w).unwrap_or_else(PoisonError::into_inner));
        }
    }
}

/// Before a blocking read of any of `reads` or a write of `write`
/// (`IO.Process.output`'s pipes and its input's pipe): in a program with
/// other contexts, wait until one of `reads` is readable or `write` is
/// writable. The caller has checked `sched::io_cooperative()` (review
/// RSIO-04).
pub(crate) fn before_ready_any(reads: &[BorrowedFd<'_>], write: Option<BorrowedFd<'_>>) {
    let mut items: Vec<PollItem<'_>> = reads
        .iter()
        .map(|&f| PollItem::new(f, Interest::READ))
        .chain(write.map(|w| PollItem::new(w, Interest::WRITE)))
        .collect();
    let _ = sched::poll_fds(&mut items, None);
}

// ---------------------------------------------------------------------------
// flock

/// How long a context waiting in `flock` waits before it tries again (the
/// lock may be released by another process, which nothing here sees);
/// doubled from `FLOCK_FIRST` up to `FLOCK_MAX`. An unlock in this process
/// wakes it at once (`flock_released`, through `sched::wake_napping`: the
/// nap is a `Wait::Sleep`, which `sched::wake` leaves alone).
const FLOCK_FIRST: Duration = Duration::from_millis(1);
const FLOCK_MAX: Duration = Duration::from_millis(16);

/// `flock(fd, op)` for a blocking `op`, cooperatively: `None` when the call
/// is to stay plain (no other context, or a non-blocking `op`).
pub(crate) fn flock(fd: &Fd, op: FlockOperation) -> Option<Result<(), i32>> {
    join_own_writers(JoinAt::End);
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
    let mut last = FlockWake::AtOnce;
    loop {
        match rustix::fs::flock(b, nb) {
            Ok(()) => {
                FLOCK_LAST.with(|l| l.set(last));
                return Some(Ok(()));
            }
            Err(Errno::WOULDBLOCK) => {
                let me = sched::current_context();
                FLOCK_WAITERS.with(|w| w.borrow_mut().push(me));
                sched::block_until(Instant::now() + nap);
                FLOCK_WAITERS.with(|w| w.borrow_mut().retain(|&c| c != me));
                let handed = FLOCK_HANDED.with(|h| {
                    let mut h = h.borrow_mut();
                    let k = h.iter().position(|&c| c == me);
                    k.map(|k| h.swap_remove(k)).is_some()
                });
                last = if handed {
                    FlockWake::Unlock
                } else {
                    FlockWake::NapEnd
                };
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
/// the contexts waiting in `flock` try again at once. They nap in
/// `block_until` (`Wait::Sleep`), so the wake is `sched::wake_napping`:
/// `sched::wake` wakes only `block_sync`'s waiters (review RW1-08), and with
/// it the handoff waited for the end of the nap, up to `FLOCK_MAX` (review
/// NEW-1 of wait-1).
pub(crate) fn flock_released() {
    let w = FLOCK_WAITERS
        .try_with(|w| std::mem::take(&mut *w.borrow_mut()))
        .unwrap_or_default();
    if !w.is_empty() && crate::sched::alive() {
        for c in w {
            sched::wake_napping(c);
            if sched::can_run(c) {
                let _ = FLOCK_HANDED.try_with(|h| h.borrow_mut().push(c));
            }
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
    join_own_writers(JoinAt::End);
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

    fn pipe_fd() -> (std::fs::File, Fd) {
        let (r, w) = std::io::pipe().unwrap();
        let w: std::os::fd::OwnedFd = w.into();
        (
            std::fs::File::from(std::os::fd::OwnedFd::from(r)),
            Fd::Owned(std::sync::Arc::new(std::fs::File::from(w))),
        )
    }

    fn read_all(mut r: std::fs::File) -> Vec<u8> {
        use std::io::Read;
        let mut v = Vec::new();
        r.read_to_end(&mut v).unwrap();
        v
    }

    /// AR-8: a hand-off returns at once, even into a full pipe; its writer
    /// writes every byte and closes the descriptor (the reader then sees end
    /// of file), and `join_writers` waits for it.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn hand_off_writes_and_closes_without_blocking_the_dropper() {
        let (r, fd) = pipe_fd();
        // fill the pipe
        let b = fd.borrow().unwrap();
        rustix::fs::fcntl_setfl(b, rustix::fs::OFlags::NONBLOCK).unwrap();
        let mut filled = 0;
        while let Ok(n) = rustix::io::write(b, &[b'x'; 4096]) {
            filled += n;
        }
        rustix::fs::fcntl_setfl(b, rustix::fs::OFlags::empty()).unwrap();
        hand_off(vec![b'y'; 100], fd);
        // the drop's thread goes on at once; the reader drains, the writer
        // finishes
        let got = read_all(r);
        assert_eq!(got.len(), filled + 100);
        assert!(got.ends_with(&[b'y'; 100]));
        join_own_writers(JoinAt::Exit);
    }

    /// leanrs's condition on AR-8: where no writer thread can start, the
    /// dropping thread writes and closes itself, with no panic and no byte
    /// lost (the hook makes the spawn fail).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn hand_off_without_a_thread_writes_in_place() {
        let (r, fd) = pipe_fd();
        FAIL_WRITER_SPAWN.with(|f| f.set(true));
        hand_off(b"in place".to_vec(), fd);
        FAIL_WRITER_SPAWN.with(|f| f.set(false));
        // written and closed before `hand_off` returned
        assert_eq!(read_all(r), b"in place");
    }

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

    /// Review NEW-1 of wait-1: `main` waits in a cooperative `flock` while
    /// a task's handle holds the lock; the task's unlock makes `main` able
    /// to run at once, not at the end of its nap (up to `FLOCK_MAX`). `main`
    /// naps whenever the task runs (the task runs only while `main` is
    /// blocked), so the check does not depend on timing.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_unlock_wakes_a_flock_waiter_at_once() {
        use crate::io::{FsMode, Handle};
        use std::cell::Cell;
        use std::rc::Rc;
        struct NoSuspend;
        impl sched::Glue for NoSuspend {
            fn suspend(&self, _: sched::Suspend<'_>) {
                panic!("the crate's unit tests never suspend a context");
            }
        }
        sched::start_with(Rc::new(NoSuspend), 1, 1 << 20);
        sched::reactor_coop_on_for_tests();
        let path = std::env::temp_dir().join(format!(
            "lean-runtime-flock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let p = path.to_str().unwrap().as_bytes().to_vec();
        let a = Handle::open(&p, FsMode::Write).unwrap();
        let b = Handle::open(&p, FsMode::Write).unwrap();
        a.lock(true).unwrap();
        let woken = Rc::new(Cell::new(None));
        let w2 = woken.clone();
        let _t = sched::spawn(
            Box::new(move || {
                a.unlock().unwrap();
                w2.set(Some(sched::main_runnable_for_tests()));
                sched::Outcome::Done
            }),
            0,
            true,
        );
        b.lock(true).unwrap();
        b.unlock().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(woken.get(), Some(true), "the unlock woke main at once");
        sched::finish();
    }

    /// HIO-06: a stream's non-blocking mode is read at every call, since
    /// another process sharing the open file description may change it. A
    /// pipe first seen in non-blocking mode cooperates once it is made
    /// blocking (a plain read would block the scheduler's only thread), and
    /// is plain again once made non-blocking (its `EAGAIN` is the result, as
    /// natively); a drop's flush takes its usual write then too.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn the_nonblocking_mode_is_read_at_every_call() {
        use rustix::fs::{fcntl_setfl, OFlags};
        use std::rc::Rc;
        struct NoSuspend;
        impl sched::Glue for NoSuspend {
            fn suspend(&self, _: sched::Suspend<'_>) {
                panic!("the crate's unit tests never suspend a context");
            }
        }
        sched::start_with(Rc::new(NoSuspend), 1, 1 << 20);
        sched::reactor_coop_on_for_tests();
        // a queued task: io is cooperative
        let _t = sched::spawn(Box::new(|| sched::Outcome::Done), 0, true);
        assert!(sched::io_cooperative());
        let (r, w) = rustix::pipe::pipe().unwrap();
        fcntl_setfl(&r, OFlags::NONBLOCK).unwrap();
        fcntl_setfl(&w, OFlags::NONBLOCK).unwrap();
        let r = Fd::Owned(std::sync::Arc::new(r.into()));
        let w = Fd::Owned(std::sync::Arc::new(w.into()));
        let mut c = Coop::UNKNOWN;
        assert!(!c.waits(&r), "non-blocking at the first call");
        assert_eq!(c, Coop::WAIT);
        fcntl_setfl(r.borrow().unwrap(), OFlags::empty()).unwrap();
        assert!(c.waits(&r), "made blocking by another process");
        fcntl_setfl(r.borrow().unwrap(), OFlags::NONBLOCK).unwrap();
        assert!(!c.waits(&r), "made non-blocking again");
        let mut cw = Coop::UNKNOWN;
        assert_eq!(write_nowait(&w, b"x", &mut cw), None);
        fcntl_setfl(w.borrow().unwrap(), OFlags::empty()).unwrap();
        assert_eq!(write_nowait(&w, b"x", &mut cw), Some(Ok(1)));
        sched::finish();
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
        // a pipe in non-blocking mode: still a pipe (the mode is read at
        // every call: `the_nonblocking_mode_is_read_at_every_call`)
        let w = Fd::Owned(std::sync::Arc::new(w.into()));
        assert_eq!(Coop::of(&w), Coop::WAIT);
        assert!(nonblocking(&w));
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
