//! `IO.FS.Handle`: open modes, the three standard streams, Lean's handle
//! primitives (`lean_io_prim_handle_*` in Lean 4.34.0's `io.cpp`) and the
//! list of open handles that the exit sequence walks.
//!
//! A [`Handle`] is a glibc `FILE` ([`CFile`]): one of the standard streams
//! (static, never closed, as Lean's persistent `stdin`, `stdout` and `stderr`
//! handles), or a file this crate opened, closed (`fclose`) when the last
//! clone of its `Handle` goes away, as Lean's handle finalizer closes it. A
//! translator keeps one `Handle` inside its own external object and drops it
//! when that object is freed, in its own order.
//!
//! Every primitive reports a failure as `decode_io_error(errno, nullptr)`
//! does, without a file name; the opening reports it with the path. A stream
//! lock poisoned by a panic is taken anyway (the model's state stays
//! consistent between its steps).
//!
//! Sources: lean2rr's `runtime/leanrt/src/fs.rs` (`open_file`, the handle
//! primitives, the open list in glibc's order) and `io.rs` (the standard
//! streams, the courtesy flush of a line-buffered stdout); leanrs's
//! `rt/leanrs_rt/src/io/handle.rs` (`FsMode`, the `Handle` and slot design,
//! the tests in `tests/io_rows.rs`).

use super::cfile::{CFile, BUSY_INPUT};
use super::error::{IoError, ENOMEM, EWOULDBLOCK};
use super::{sys, ByteSink};
use rustix::fd::OwnedFd;
use rustix::fs::{FlockOperation, OFlags};
use std::collections::BTreeMap;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Lean's `IO.FS.Mode` (`Init/System/IO.lean`), its constructors in Lean's
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsMode {
    Read,
    Write,
    WriteNew,
    ReadWrite,
    Append,
}

impl FsMode {
    /// The mode of Lean's constructor index (0 to 4).
    pub fn from_index(i: u8) -> Option<FsMode> {
        Some(match i {
            0 => FsMode::Read,
            1 => FsMode::Write,
            2 => FsMode::WriteNew,
            3 => FsMode::ReadWrite,
            4 => FsMode::Append,
            _ => return None,
        })
    }

    /// The `open` flags of `lean_io_prim_handle_mk`, `O_CLOEXEC` included.
    fn open_flags(self) -> OFlags {
        OFlags::CLOEXEC
            | match self {
                FsMode::Read => OFlags::RDONLY,
                FsMode::Write => OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
                FsMode::WriteNew => OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::EXCL,
                FsMode::ReadWrite => OFlags::RDWR,
                FsMode::Append => OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND,
            }
    }
}

/// glibc's `stdin`, `stdout` and `stderr`.
pub(crate) static STDIN: Mutex<CFile> = Mutex::new(CFile::std(0));
pub(crate) static STDOUT: Mutex<CFile> = Mutex::new(CFile::std(1));
pub(crate) static STDERR: Mutex<CFile> = Mutex::new(CFile::std(2));

/// A stream's lock, held (`flockfile`): the stream's `FILE`, through
/// `Deref`. Taken even if a panic poisoned it. In a program with tasks
/// (feature `sched`) the lock is taken cooperatively and released with a
/// wake-up of whoever waits for it (`io::coop`, "Stream locks").
#[derive(Debug)]
pub struct StreamGuard<'a> {
    g: MutexGuard<'a, CFile>,
    /// The stream's key when taken through `coop::lock` (0: a plain lock).
    #[cfg(feature = "sched")]
    key: usize,
}

impl<'a> StreamGuard<'a> {
    #[inline]
    pub(crate) fn plain(g: MutexGuard<'a, CFile>) -> StreamGuard<'a> {
        StreamGuard {
            g,
            #[cfg(feature = "sched")]
            key: 0,
        }
    }

    #[cfg(feature = "sched")]
    pub(crate) fn tracked(g: MutexGuard<'a, CFile>, key: usize) -> StreamGuard<'a> {
        StreamGuard { g, key }
    }
}

impl std::ops::Deref for StreamGuard<'_> {
    type Target = CFile;
    #[inline]
    fn deref(&self) -> &CFile {
        &self.g
    }
}

impl std::ops::DerefMut for StreamGuard<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut CFile {
        &mut self.g
    }
}

impl Drop for StreamGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        // what the holder was doing ends with the hold (LB-29)
        self.g.unmark();
        // The waiters are woken before `g` unlocks, which is fine: waking
        // switches nothing, and `g` drops right after this.
        #[cfg(feature = "sched")]
        if self.key != 0 {
            super::coop::released(self.key);
        }
    }
}

/// A stream's lock, taken even if a panic poisoned it.
#[inline]
pub(crate) fn lock(m: &Mutex<CFile>) -> StreamGuard<'_> {
    #[cfg(feature = "sched")]
    if crate::sched::coop_possible() {
        return super::coop::lock(m);
    }
    StreamGuard::plain(m.lock().unwrap_or_else(PoisonError::into_inner))
}

/// A stream's lock if no one holds it.
#[inline]
pub(crate) fn try_lock(m: &Mutex<CFile>) -> Option<StreamGuard<'_>> {
    #[cfg(feature = "sched")]
    if crate::sched::coop_possible() {
        return super::coop::try_lock(m);
    }
    match m.try_lock() {
        Ok(g) => Some(StreamGuard::plain(g)),
        Err(std::sync::TryLockError::Poisoned(p)) => Some(StreamGuard::plain(p.into_inner())),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// An open file: its `FILE`, closed when the last `Handle` goes away (and
/// the open list's slot with it).
#[derive(Debug)]
pub(crate) struct FileStream {
    pub(crate) file: Mutex<CFile>,
    /// The descriptor, reachable without the lock (`fileno(fp)`), for the
    /// calls that take no `FILE` lock natively (`flock`, `isatty`).
    fd: sys::Fd,
    /// Its key in the open list ([`OPEN`]): the order of its opening.
    serial: u64,
    /// What the holder of `file`'s lock is doing (LB-29), readable without
    /// the lock: a clone of the stream's cell.
    busy: Option<Arc<AtomicU8>>,
}

impl FileStream {
    /// Whether the holder of the stream's lock is blocked reading it (LB-29).
    pub(crate) fn busy_reading(&self) -> bool {
        self.busy
            .as_ref()
            .is_some_and(|c| c.load(AtomicOrdering::Acquire) == BUSY_INPUT)
    }
}

/// The open files (see [`OPEN`]).
struct OpenList {
    /// Each open file under its serial, so oldest first: glibc's
    /// `_IO_list_all`, reversed (new streams are linked at its head, and the
    /// exit walks it from there).
    files: BTreeMap<u64, Arc<FileStream>>,
    /// The next file's serial (a `u64` never wraps: one per opening).
    next: u64,
}

/// The open files. Each slot is a strong reference, held besides the
/// stream's `Handle`s (no non-owning reference: leanrs's ownership rule S4
/// forbids std's in runtime code). Every other reference is let go through
/// [`release`], under this lock, so the one that leaves the slot alone with
/// it sees a count of 2 and empties the slot, and the stream closes.
///
/// A map keyed by the opening's serial, not a `Vec` searched at each
/// release (AR-7): opening, closing and the exit's walk keep the `Vec`'s
/// order, and closing is O(log N), where the `Vec`'s search and removal
/// made the drop of N open handles O(N^2) (leanrs: 200000 handles in 7.91 s
/// against native's 0.66 s, which unlinks a `FILE` from glibc's list).
static OPEN: Mutex<OpenList> = Mutex::new(OpenList {
    files: BTreeMap::new(),
    next: 0,
});

impl Drop for FileStream {
    fn drop(&mut self) {
        let file = self.file.get_mut().unwrap_or_else(PoisonError::into_inner);
        // In a no-suspend scope (a translator's drop path), a flush that
        // would wait for a pipe (perhaps one this program drains) hands the
        // rest and the descriptor to a writer thread, which closes it
        // (review RSIO-09; AR-8). The descriptor closes there, so a context
        // waiting in `flock` sees the release when it next looks (within
        // 16 ms).
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() && crate::sched::in_no_suspend() && !file.flush_nowait() {
            let (bytes, fd) = file.hand_off();
            super::coop::hand_off(bytes, fd);
            return;
        }
        file.close();
        // Closing the descriptor released its `flock` lock, if any.
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() {
            super::coop::flock_released();
        }
    }
}

/// Lets go of a reference to an open file: when only the open list's slot
/// holds it besides (a strong count of 2, under the list's lock, where every
/// release happens, so two releases never both see 3), the slot is removed
/// and the file closes, as Lean's finalizer `fclose`s it, outside the lock
/// (closing writes pending output, which may wait on a pipe).
fn release(f: Arc<FileStream>) {
    // `f` and the open list's slot, which stays while any other reference
    // does.
    debug_assert!(Arc::strong_count(&f) >= 2);
    let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    if Arc::strong_count(&f) == 2 {
        let slot = open.files.remove(&f.serial);
        debug_assert!(
            slot.as_ref().is_some_and(|g| Arc::ptr_eq(g, &f)),
            "an open file's last reference has its slot"
        );
        drop(open);
        drop(slot);
        drop(f);
        return;
    }
    drop(f);
}

/// The open files, newest first (the order of `_IO_flush_all` and
/// `_IO_unbuffer_all`); dropping it lets go of them through [`release`], so
/// a file whose last `Handle` went away meanwhile closes then.
pub(crate) struct OpenFiles(Vec<Arc<FileStream>>);

impl std::ops::Deref for OpenFiles {
    type Target = [Arc<FileStream>];
    fn deref(&self) -> &[Arc<FileStream>] {
        &self.0
    }
}

impl Drop for OpenFiles {
    fn drop(&mut self) {
        for f in std::mem::take(&mut self.0) {
            release(f);
        }
    }
}

/// The open files, newest first (see [`OpenFiles`]).
pub(crate) fn open_files_newest_first() -> OpenFiles {
    let open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    OpenFiles(open.files.values().rev().cloned().collect())
}

/// `IO.FS.Handle`: a standard stream or an open file (see the module
/// comment). Clones name the same stream.
#[derive(Clone, Debug)]
pub struct Handle(Repr);

/// Every drop of a file's `Handle` takes the [`OPEN`] lock (in
/// [`release`]), also when other clones remain. Handle drops are rare (a
/// translator drops one when it frees its Lean handle object), and reads and
/// writes take only the stream's own lock.
impl Drop for Handle {
    fn drop(&mut self) {
        if let Repr::File(_) = self.0 {
            if let Repr::File(f) = std::mem::replace(&mut self.0, Repr::Std(0)) {
                release(f);
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Repr {
    /// glibc's `stdin` (0), `stdout` (1) or `stderr` (2).
    Std(u8),
    File(Arc<FileStream>),
}

/// The `FILE` of a standard stream.
fn std_stream(n: u8) -> &'static Mutex<CFile> {
    match n {
        0 => &STDIN,
        1 => &STDOUT,
        _ => &STDERR,
    }
}

/// `lean_alloc_sarray_would_overflow(1, n)`: a byte array of `n` bytes does
/// not fit in memory with Lean's 24-byte header.
#[inline]
pub fn sarray_would_overflow(n: usize) -> bool {
    n > usize::MAX - 24
}

/// `lean_io_prim_handle_read`'s first check, before the array is allocated:
/// `n` bytes whose array would overflow is `ENOMEM` (`resourceExhausted`).
/// A translator calls it, allocates `n` bytes in its own `ByteArray`, then
/// calls [`Handle::read`] on them.
pub fn check_read_size(n: usize) -> Result<(), IoError> {
    if sarray_would_overflow(n) {
        Err(IoError::decode_io_error(ENOMEM, None))
    } else {
        Ok(())
    }
}

/// `decode_io_error(errno, nullptr)` of a primitive's failure.
fn os(e: i32) -> IoError {
    IoError::decode_io_error(e, None)
}

/// `_IO_new_file_underflow`'s courtesy flush: reading a line-buffered or
/// unbuffered stream first writes a line-buffered stdout's pending output.
/// (Never called with `STDOUT` locked: stdout cannot read.)
pub(crate) fn flush_line_buffered_stdout() {
    let mut out = lock(&STDOUT);
    if out.is_line_buffered() {
        let _ = out.flush_pending();
    }
}

impl Handle {
    /// glibc's `stdin` as a handle (`IO.getStdin`'s default stream).
    pub fn stdin() -> Handle {
        Handle(Repr::Std(0))
    }

    /// glibc's `stdout` as a handle.
    pub fn stdout() -> Handle {
        Handle(Repr::Std(1))
    }

    /// glibc's `stderr` as a handle (unbuffered).
    pub fn stderr() -> Handle {
        Handle(Repr::Std(2))
    }

    /// `IO.FS.Handle.mk` (`lean_io_prim_handle_mk`): a path holding a NUL byte
    /// is `mk_embedded_nul_error`; then `open` with the mode's flags,
    /// `O_CLOEXEC` and permissions `0666`, whose failure is decoded with the
    /// path, and `fdopen` with `"r"`, `"w"`, `"w"`, `"r+"` or `"a"`.
    pub fn open(path: &[u8], mode: FsMode) -> Result<Handle, IoError> {
        super::effect_point();
        if path.contains(&0) {
            return Err(IoError::embedded_nul(path));
        }
        match sys::open(path, mode.open_flags()) {
            Ok(fd) => Ok(Handle::fdopen(fd, mode)),
            Err(e) => Err(IoError::decode_io_error(e, Some(path))),
        }
    }

    /// `fdopen(fd, mode)` and Lean's `io_wrap_handle`: a handle over a
    /// descriptor opened elsewhere (a temporary file, a child's pipe), which
    /// it owns and closes. It joins the open list, newest, as `fdopen` links
    /// the new `FILE` into glibc's list.
    pub fn fdopen(fd: OwnedFd, mode: FsMode) -> Handle {
        Handle::register(CFile::fdopen(fd, mode))
    }

    /// A write-only handle over the non-blocking write end of a pipe whose
    /// reader never reads, for the standard input of a child process that
    /// could not start: a write into the full pipe fails with `EPIPE`
    /// ([`CFile::fdopen_bounded_pipe`]). The caller sets `O_NONBLOCK` on `fd`.
    pub fn fdopen_bounded_pipe(fd: OwnedFd) -> Handle {
        Handle::register(CFile::fdopen_bounded_pipe(fd))
    }

    fn register(file: CFile) -> Handle {
        let fd = file.descriptor();
        let busy = file.busy_arc();
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        let serial = open.next;
        open.next += 1;
        let f = Arc::new(FileStream {
            file: Mutex::new(file),
            fd,
            serial,
            busy,
        });
        open.files.insert(serial, f.clone());
        drop(open);
        Handle(Repr::File(f))
    }

    /// The handle's `FILE`, locked (`flockfile`). A handle's descriptor
    /// closes only when its open file goes away (the last clone of the
    /// `Handle` dropped), never through the guard: the file keeps a shared
    /// clone of the descriptor for the calls made without the stream's lock
    /// (review RIO1-11). In a program with tasks (feature `sched`), the guard
    /// may be held across any wait: every context switch records it as held
    /// by the suspended context, and another context that wants the stream
    /// waits for it. The exception is a guard taken before the program's
    /// first task, promise, timer or watch (docs/sched.md, "Stream locks").
    #[inline]
    pub fn file(&self) -> StreamGuard<'_> {
        match &self.0 {
            Repr::Std(n) => lock(std_stream(*n)),
            Repr::File(f) => lock(&f.file),
        }
    }

    /// The open file behind the handle (`None` for a standard stream).
    #[cfg(test)]
    pub(crate) fn file_stream(&self) -> Option<&Arc<FileStream>> {
        match &self.0 {
            Repr::Std(_) => None,
            Repr::File(f) => Some(f),
        }
    }

    /// Whether two handles are the same stream.
    pub fn ptr_eq(&self, other: &Handle) -> bool {
        match (&self.0, &other.0) {
            (Repr::Std(a), Repr::Std(b)) => a == b,
            (Repr::File(a), Repr::File(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// `Handle.putStr` (`lean_io_prim_handle_put_str`): `fwrite` of the
    /// string's bytes.
    #[inline]
    pub fn put_str(&self, s: &[u8]) -> Result<(), IoError> {
        self.file().put(s).map_err(os)
    }

    /// `Handle.write` (`lean_io_prim_handle_write`): `fwrite` of the bytes.
    #[inline]
    pub fn write(&self, b: &[u8]) -> Result<(), IoError> {
        self.file().put(b).map_err(os)
    }

    /// `Handle.flush` (`lean_io_prim_handle_flush`): `fflush`.
    pub fn flush(&self) -> Result<(), IoError> {
        self.file().flush().map_err(os)
    }

    /// `Handle.read` (`lean_io_prim_handle_read`) into `out`, the `n` bytes
    /// the caller allocated after [`check_read_size`]: `fread`, the count
    /// read (0 at end of file, after `clearerr`). `n = 0` reads nothing.
    /// LB-02: output pending on the handle is written first.
    #[inline]
    pub fn read(&self, out: &mut [u8]) -> Result<usize, IoError> {
        if out.is_empty() {
            return Ok(0);
        }
        self.file().read(out).map_err(os)
    }

    /// [`Handle::read`] into uninitialized memory: the `n` bytes of the
    /// translator's new `ByteArray`, allocated (after [`check_read_size`]) as
    /// Lean allocates it, without a zero pass. The count read; `out[..count]`
    /// is initialized.
    #[inline]
    pub fn read_uninit(&self, out: &mut [MaybeUninit<u8>]) -> Result<usize, IoError> {
        if out.is_empty() {
            return Ok(0);
        }
        self.file().read_uninit(out).map_err(os)
    }

    /// [`Handle::read`] of up to `n` bytes appended to `out`, a translator's
    /// `Vec`-based `ByteArray` (call [`check_read_size`] first). The count
    /// read. The `Vec` grows with the bytes read (nothing is reserved for
    /// `n`); from a regular file nothing is zeroed, from a pipe or a standard
    /// descriptor each direct `read(2)` goes into a zeroed window of at most
    /// the pipe's capacity or 64 KiB (the module comment of
    /// `lean_runtime::io` has the rule and an example).
    #[inline]
    pub fn read_vec(&self, n: usize, out: &mut Vec<u8>) -> Result<usize, IoError> {
        if n == 0 {
            return Ok(0);
        }
        self.file().read_vec(n, out).map_err(os)
    }

    /// `Handle.getLine` (`lean_io_prim_handle_get_line`): the bytes up to and
    /// including the first `\n` (or to end of file) appended to `out`. On
    /// `Err` the caller drops what was appended, as Lean loses the line. The
    /// caller decodes the bytes as Lean's `mk_string` does (lossily). Once
    /// `out` has stopped ([`ByteSink::stopped`]), it reads no further and
    /// returns `ENOMEM`'s error (`resource exhausted`), which the glue does
    /// not use: it ends the process with its out-of-memory report
    /// ([`super::cfile::CFile::get_line`]; AR-19).
    #[inline]
    pub fn get_line<S: ByteSink + ?Sized>(&self, out: &mut S) -> Result<(), IoError> {
        self.file().get_line(out).map_err(os)
    }

    /// `Handle.isEof` (`lean_io_prim_handle_is_eof`, `feof`).
    pub fn is_eof(&self) -> bool {
        self.file().is_eof()
    }

    /// `Handle.isTty` (`lean_io_prim_handle_is_tty`, `isatty`, errors
    /// ignored).
    pub fn is_tty(&self) -> bool {
        self.fileno().isatty()
    }

    /// `Handle.rewind` (`lean_io_prim_handle_rewind`, `fseek(fp, 0,
    /// SEEK_SET)`). LB-09: a target inside the buffer is served from it.
    pub fn rewind(&self) -> Result<(), IoError> {
        self.file().rewind().map_err(os)
    }

    /// `Handle.truncate` (`lean_io_prim_handle_truncate`,
    /// `ftruncate(fileno(fp), ftello(fp))`, without flushing).
    pub fn truncate(&self) -> Result<(), IoError> {
        self.file().truncate().map_err(os)
    }

    /// `flock(fileno(fp), op)`: natively it takes no `FILE` lock, so it runs
    /// on the descriptor without the stream's lock, which a blocking read of
    /// another task may hold: other operations on the handle and the exit go
    /// on while a task waits in `flock`, and `flock` does not wait for them
    /// (review RIO1-01).
    fn flock(&self, op: FlockOperation) -> Result<(), i32> {
        let fd = self.fileno();
        // In a program with tasks, a wait for the lock lets the others run
        // (sched-io).
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() {
            if let Some(r) = super::coop::flock(&fd, op) {
                return r;
            }
        }
        fd.flock(op)
    }

    /// `fileno(fp)`, without the stream's lock: a standard descriptor, or
    /// the file's shared descriptor (it stays open while the clone lives).
    fn fileno(&self) -> sys::Fd {
        match &self.0 {
            Repr::Std(n) => sys::Fd::Std(*n),
            Repr::File(f) => f.fd.clone(),
        }
    }

    /// `Handle.lock` (`lean_io_prim_handle_lock`, `flock` with `LOCK_EX` or
    /// `LOCK_SH`), waiting without holding the stream.
    pub fn lock(&self, exclusive: bool) -> Result<(), IoError> {
        let op = if exclusive {
            FlockOperation::LockExclusive
        } else {
            FlockOperation::LockShared
        };
        self.flock(op).map_err(os)
    }

    /// `Handle.tryLock` (`lean_io_prim_handle_try_lock`, `flock` with
    /// `LOCK_NB`): `false` when the lock is held elsewhere (`EWOULDBLOCK`).
    pub fn try_lock(&self, exclusive: bool) -> Result<bool, IoError> {
        let op = if exclusive {
            FlockOperation::NonBlockingLockExclusive
        } else {
            FlockOperation::NonBlockingLockShared
        };
        match self.flock(op) {
            Ok(()) => Ok(true),
            Err(EWOULDBLOCK) => Ok(false),
            Err(e) => Err(os(e)),
        }
    }

    /// `Handle.unlock` (`lean_io_prim_handle_unlock`, `flock` with
    /// `LOCK_UN`).
    pub fn unlock(&self) -> Result<(), IoError> {
        let r = self.flock(FlockOperation::Unlock).map_err(os);
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() {
            super::coop::flock_released();
        }
        r
    }
}
