//! A model of glibc's `FILE` (libio, glibc 2.39), which native Lean uses for
//! the standard streams and every `IO.FS.Handle`.
//!
//! Lean programs observe stdio through what reaches file descriptors and
//! when (other readers of a file, `2>&1` interleaving, pipes), through file
//! positions (`rewind`, `truncate`, the stdin a program leaves to the next
//! process) and through the sticky end-of-file and error indicators. So this
//! module follows libio's algorithms closely: one buffer shared by reading and
//! writing, the get and put areas, the cached file offset, the same system
//! calls in the same order (hence the same `errno`s). Function names are
//! libio's (`fileops.c`, `genops.c`): `xsputn`, `overflow`, `new_do_write`,
//! `underflow`, `uflow`, `xsgetn`, `sync`, `seekoff`, `do_ftell`,
//! `doallocate`. The Lean-level operations ([`CFile::put`], [`CFile::read`],
//! [`CFile::get_line`], [`CFile::flush`], [`CFile::rewind`],
//! [`CFile::truncate`]) are those of `lean_io_prim_handle_*` in Lean's
//! `io.cpp`.
//!
//! Buffer "pointers" are indices into `buf`; `has_buf` is false while
//! glibc's buffer is NULL. Wide orientation, backup areas and markers are not
//! used by Lean and are not modelled.
//!
//! **LB-02** (`docs/lean-bugs.md`): glibc's `_IO_file_xsgetn` resets the put
//! area before a direct read (a read of at least one buffer), so output
//! pending on the stream is lost. Here such a read with output pending calls
//! `fflush` first (`_IO_new_file_sync`), so the bytes are written and the
//! read starts at the stream's position; everything else on that path is
//! glibc's. Smaller reads and `getLine`
//! already write them natively (`_IO_switch_to_get_mode`).
//!
//! Source: lean2rr's `runtime/leanrt/src/cfile.rs` (matches native on the
//! leanrs findings A811 and A812, and a differential test against glibc,
//! `tests/cfile_glibc.rs`), rewritten without `unsafe`: system calls through
//! rustix (`super::sys`), the modelled `errno` instead of C's, reads into the
//! caller's `&mut [u8]`.

use super::error::{errno, set_errno, EAGAIN, EBADF, EINVAL, ENOMEM, EPIPE, ESPIPE};
use super::handle::FsMode;
use super::sys::{Fd, ReadDest, VecDest};
use super::ByteSink;
use rustix::fd::OwnedFd;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// `_IO_UNBUFFERED`.
const UNBUFFERED: u32 = 0x2;
/// `_IO_NO_READS`: opened for writing only.
const NO_READS: u32 = 0x4;
/// `_IO_NO_WRITES`: opened for reading only.
const NO_WRITES: u32 = 0x8;
/// `_IO_EOF_SEEN`.
const EOF_SEEN: u32 = 0x10;
/// `_IO_ERR_SEEN`.
const ERR_SEEN: u32 = 0x20;
/// `_IO_LINE_BUF`.
const LINE_BUF: u32 = 0x200;
/// `_IO_CURRENTLY_PUTTING`.
const CURRENTLY_PUTTING: u32 = 0x800;
/// `_IO_IS_APPENDING`.
const IS_APPENDING: u32 = 0x1000;

const EOF: i32 = -1;
const POS_BAD: i64 = -1;
const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;
/// glibc's `BUFSIZ`.
const BUFSIZ: usize = 8192;

/// What the holder of a stream's lock is doing, readable without the lock
/// (LB-29: the exit's flush waits for a stream whose holder writes, and
/// skips one whose holder is blocked reading): nothing that blocks...
pub(crate) const BUSY_IDLE: u8 = 0;
/// ... a read of the descriptor (or the cooperative wait before it) ...
pub(crate) const BUSY_INPUT: u8 = 1;
/// ... or a write of it.
pub(crate) const BUSY_OUTPUT: u8 = 2;

/// The state of the three standard streams (their `FILE`s are statics).
static STD_BUSY: [AtomicU8; 3] = [const { AtomicU8::new(BUSY_IDLE) }; 3];

/// The state cell of standard stream `n` (0 to 2).
pub(crate) fn std_busy(n: u8) -> &'static AtomicU8 {
    &STD_BUSY[usize::from(n.min(2))]
}

/// A stream's buffer size from its descriptor's `st_blksize`, as glibc's
/// `_IO_file_doallocate` (`libio/filedoalloc.c`) computes it: `BUFSIZ`, or
/// `st_blksize` when `0 < st_blksize < BUFSIZ`. From leanrs's
/// `rt/leanrs_rt/src/io/block.rs`.
pub fn buffer_size(blksize: i64) -> usize {
    if blksize > 0 && blksize < BUFSIZ as i64 {
        blksize as usize
    } else {
        BUFSIZ
    }
}

/// One glibc `FILE` (see the module comment).
#[derive(Debug)]
pub struct CFile {
    fd: Fd,
    flags: u32,
    buf: Vec<u8>,
    has_buf: bool,
    rb: usize,
    rp: usize,
    re: usize,
    wb: usize,
    wp: usize,
    we: usize,
    /// The cached file offset (`_offset`), `POS_BAD` when unknown.
    offset: i64,
    /// The stream has been used (`_mode != 0`).
    used: bool,
    /// A write that fails with `EAGAIN` reports `EPIPE`
    /// ([`CFile::fdopen_bounded_pipe`]).
    eagain_is_epipe: bool,
    /// The last `new_do_write` returned at its seek back over read-ahead
    /// (`ESPIPE` on a FIFO opened `readWrite`), writing nothing and setting
    /// no error indicator.
    seek_failed: bool,
    /// The descriptor is a regular file (known once the buffer exists).
    regular: bool,
    /// What the descriptor is for the cooperative path (sched-io).
    #[cfg(feature = "sched")]
    coop: super::coop::Coop,
    /// The state of an opened stream's holder (`None` for a standard
    /// stream, whose cell is `STD_BUSY`): see [`BUSY_IDLE`].
    busy: Option<Arc<AtomicU8>>,
    /// The cell is not `BUSY_IDLE` (set and read under the lock), so the
    /// guard's release resets it only then: no atomic store on the paths
    /// that never block.
    marked: bool,
}

/// The `fdopen` flags of a mode (`lean_io_prim_handle_mk`): `read` is `"r"`,
/// `write` and `writeNew` `"w"`, `readWrite` `"r+"`, `append` `"a"`.
fn mode_flags(mode: FsMode) -> u32 {
    match mode {
        FsMode::Read => NO_WRITES,
        FsMode::Write | FsMode::WriteNew => NO_READS,
        FsMode::ReadWrite => 0,
        FsMode::Append => NO_READS | IS_APPENDING,
    }
}

impl CFile {
    const fn with(fd: Fd, flags: u32) -> CFile {
        CFile {
            fd,
            flags,
            buf: Vec::new(),
            has_buf: false,
            rb: 0,
            rp: 0,
            re: 0,
            wb: 0,
            wp: 0,
            we: 0,
            offset: POS_BAD,
            used: false,
            eagain_is_epipe: false,
            seek_failed: false,
            regular: false,
            #[cfg(feature = "sched")]
            coop: super::coop::Coop::UNKNOWN,
            busy: None,
            marked: false,
        }
    }

    /// The cell holding what this stream's holder is doing (LB-29).
    fn busy_cell(&self) -> Option<&AtomicU8> {
        match (&self.busy, &self.fd) {
            (Some(c), _) => Some(c),
            (None, Fd::Std(n)) => Some(std_busy(*n)),
            _ => None,
        }
    }

    /// Whether this is glibc's `stderr` (the guard's holder tracking,
    /// [`super::handle::stderr_held_here`]).
    #[inline]
    pub(crate) fn is_stderr(&self) -> bool {
        matches!(self.fd, Fd::Std(2))
    }

    /// A clone of an opened stream's cell, for the open-file list.
    pub(crate) fn busy_arc(&self) -> Option<Arc<AtomicU8>> {
        self.busy.clone()
    }

    /// Record what the holder is about to block in.
    #[inline]
    fn mark(&mut self, what: u8) {
        if let Some(c) = self.busy_cell() {
            c.store(what, Ordering::Release);
        }
        self.marked = true;
    }

    /// A read of the descriptor has returned: the holder is no longer
    /// blocked reading (one store per read; review RFX1-15: a hold that read,
    /// then buffered output and waits, is not taken for a reader).
    #[inline]
    fn read_done(&mut self) {
        if let Some(c) = self.busy_cell() {
            c.store(BUSY_IDLE, Ordering::Release);
        }
        self.marked = false;
    }

    /// The holder lets go of the stream (`StreamGuard`'s drop): its state is
    /// idle again.
    #[inline]
    pub(crate) fn unmark(&mut self) {
        if self.marked {
            if let Some(c) = self.busy_cell() {
                c.store(BUSY_IDLE, Ordering::Release);
            }
            self.marked = false;
        }
    }

    /// glibc's `stdin` (0), `stdout` (1) or `stderr` (2, unbuffered).
    pub const fn std(fd: u8) -> CFile {
        let flags = match fd {
            0 => NO_WRITES,
            1 => NO_READS,
            _ => NO_READS | UNBUFFERED,
        };
        CFile::with(Fd::Std(fd), flags)
    }

    /// `fdopen(fd, mode)` with the C mode of a Lean `IO.FS.Mode` (`"r"`, `"w"`,
    /// `"w"`, `"r+"`, `"a"`); the stream holds `fd`, which closes when the
    /// stream is dropped (`fclose`) and no `Handle` keeps a clone of it.
    /// As glibc's `fdopen` of a descriptor that has `O_APPEND` already, it
    /// does not seek for `"a"`, and the cached offset starts unknown;
    /// `Handle::open` moves an `append` descriptor of a regular file to the
    /// end first (LB-46).
    pub fn fdopen(fd: OwnedFd, mode: FsMode) -> CFile {
        let mut f = CFile::with(
            Fd::Owned(std::sync::Arc::new(std::fs::File::from(fd))),
            mode_flags(mode),
        );
        f.busy = Some(Arc::new(AtomicU8::new(BUSY_IDLE)));
        f
    }

    /// A write-only stream (mode `write`) over the non-blocking write end of a
    /// pipe whose reader never reads: the standard input of a child process
    /// that could not start. Once the pipe is full, a write fails with `EPIPE`
    /// where the descriptor reports `EAGAIN`, as natively a write fails once
    /// Lean's failed child has exited. The caller sets `O_NONBLOCK` on `fd`.
    /// From leanrs's `handle_of_bounded_pipe` (`full_is_epipe`).
    pub fn fdopen_bounded_pipe(fd: OwnedFd) -> CFile {
        let mut f = CFile::fdopen(fd, FsMode::Write);
        f.eagain_is_epipe = true;
        f
    }

    /// The put area's bytes not yet written (`buf[wb..wp]` in put mode,
    /// otherwise none): what a forked child that cannot start writes from its
    /// copy of the parent's stdout buffer. From leanrs's `stdout_pending`.
    pub fn pending_output(&self) -> &[u8] {
        if self.in_put_mode() && self.wp > self.wb {
            &self.buf[self.wb..self.wp]
        } else {
            &[]
        }
    }

    /// `fileno`: the descriptor, `-1` once closed.
    pub fn fileno(&self) -> i32 {
        self.fd.raw()
    }

    #[inline(always)]
    fn bufsize(&self) -> usize {
        self.buf.len()
    }

    #[inline(always)]
    fn setg(&mut self, b: usize, p: usize, e: usize) {
        self.rb = b;
        self.rp = p;
        self.re = e;
    }

    #[inline(always)]
    fn setp(&mut self, p: usize, e: usize) {
        self.wb = p;
        self.wp = p;
        self.we = e;
    }

    #[inline(always)]
    fn in_put_mode(&self) -> bool {
        self.flags & CURRENTLY_PUTTING != 0
    }

    /// `feof`.
    pub fn is_eof(&self) -> bool {
        self.flags & EOF_SEEN != 0
    }

    /// `ferror`.
    pub fn is_err(&self) -> bool {
        self.flags & ERR_SEEN != 0
    }

    /// Line buffered (a terminal), known once the buffer exists.
    pub fn is_line_buffered(&self) -> bool {
        self.flags & LINE_BUF != 0
    }

    /// `clearerr`.
    fn clearerr(&mut self) {
        self.flags &= !(EOF_SEEN | ERR_SEEN);
    }

    // ---- allocation ----

    /// `_IO_file_doallocate`: `st_blksize` bytes (at most `BUFSIZ`), line
    /// buffered on a terminal (`DEV_TTY_P`: a pty slave, majors 136 to 143,
    /// or `local_isatty`, which keeps `errno`). A failing `fstat` leaves its
    /// `errno` and gives `BUFSIZ`.
    ///
    /// Under Miri, which cannot run `fstat`, the buffer is always `BUFSIZ`
    /// bytes and nothing else is asked of the descriptor: never line
    /// buffered (Miri's isolation has no terminal), never marked regular
    /// (a read then goes through zeroed windows, with the same result), and
    /// `errno` untouched. Without it, any test that prints through a buffered
    /// stream stops Miri (leanrs's tests do). A seek from the end still asks
    /// `fstat`, so it does not run under Miri.
    fn doallocate(&mut self) {
        #[cfg(not(miri))]
        let size = self.probe_descriptor();
        #[cfg(miri)]
        let size = BUFSIZ;
        self.buf = vec![0u8; size];
        self.has_buf = true;
    }

    /// `doallocate`'s `fstat` of the descriptor: the buffer size, and the
    /// line buffering and `regular` mark it implies.
    #[cfg(not(miri))]
    fn probe_descriptor(&mut self) -> usize {
        let Ok(st) = self.fd.fstat() else {
            return BUFSIZ;
        };
        self.regular = st.st_mode & 0o170000 == 0o100000;
        if st.st_mode & 0o170000 == 0o020000 {
            let major = rustix::fs::major(st.st_rdev);
            if (136..=143).contains(&major) || self.fd.isatty_keep_errno() {
                self.flags |= LINE_BUF;
            }
        }
        buffer_size(st.st_blksize as i64)
    }

    /// `_IO_doallocbuf`: unbuffered streams get a one-byte buffer.
    fn doallocbuf(&mut self) {
        if self.has_buf {
            return;
        }
        if self.flags & UNBUFFERED == 0 {
            self.doallocate();
        } else {
            self.buf = vec![0u8; 1];
            self.has_buf = true;
        }
        self.setg(0, 0, 0);
        self.setp(0, 0);
    }

    // ---- system calls ----

    /// Before a `read(2)` that may block: in a program with tasks, wait until
    /// the descriptor is readable, letting the other contexts run (sched-io,
    /// `io::coop`); the `read(2)` itself is the same.
    #[inline]
    fn before_read(&mut self) {
        self.mark(BUSY_INPUT);
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() {
            super::coop::before_read(&self.fd, &mut self.coop);
        }
    }

    /// One `write(2)` of `data`: in a program with tasks, without blocking
    /// the other contexts (sched-io, `io::coop`).
    #[inline]
    fn write_once(&mut self, data: &[u8]) -> Result<usize, i32> {
        self.mark(BUSY_OUTPUT);
        #[cfg(feature = "sched")]
        if crate::sched::coop_possible() {
            if let Some(r) = super::coop::write_once(&self.fd, data, &mut self.coop) {
                return r;
            }
        }
        self.fd.write(data)
    }

    /// `_IO_new_file_write`: write all of `data`; a failure sets the error
    /// indicator. Returns the number of bytes written.
    fn syswrite(&mut self, data: &[u8]) -> usize {
        let mut done = 0;
        while done < data.len() {
            match self.write_once(&data[done..]) {
                Ok(n) => done += n,
                Err(e) => {
                    if e == EAGAIN && self.eagain_is_epipe {
                        set_errno(EPIPE);
                    }
                    self.flags |= ERR_SEEN;
                    break;
                }
            }
        }
        if self.offset >= 0 {
            self.offset += done as i64;
        }
        done
    }

    fn sysseek(&self, off: i64, whence: i32) -> i64 {
        self.fd.lseek(off, whence).unwrap_or(POS_BAD)
    }

    // ---- writing ----

    /// `new_do_write`: write `to_do` bytes, from the buffer at `from` (or from
    /// `user` when given), after moving the descriptor back over read-ahead;
    /// then empty the buffer. A failed seek returns before the reset, so the
    /// bytes stay buffered (and no error indicator is set).
    fn new_do_write(&mut self, from: usize, user: Option<&[u8]>, to_do: usize) -> usize {
        self.seek_failed = false;
        if self.flags & IS_APPENDING != 0 {
            self.offset = POS_BAD;
        } else if self.re != self.wb {
            let np = self.sysseek(self.wb as i64 - self.re as i64, SEEK_CUR);
            if np == POS_BAD {
                self.seek_failed = true;
                return 0;
            }
            self.offset = np;
        }
        let count = match user {
            Some(d) => self.syswrite(&d[..to_do]),
            None => {
                let b = std::mem::take(&mut self.buf);
                let c = self.syswrite(&b[from..from + to_do]);
                self.buf = b;
                c
            }
        };
        self.setg(0, 0, 0);
        self.wb = 0;
        self.wp = 0;
        self.we = if self.flags & (LINE_BUF | UNBUFFERED) != 0 {
            0
        } else {
            self.bufsize()
        };
        count
    }

    /// `_IO_do_write` of the pending output (`_IO_do_flush`).
    fn do_flush(&mut self) -> i32 {
        let to_do = self.wp - self.wb;
        if to_do == 0 || self.new_do_write(self.wb, None, to_do) == to_do {
            0
        } else {
            EOF
        }
    }

    /// `_IO_new_file_overflow(f, ch)`; `ch = None` is `EOF` (flush).
    fn overflow(&mut self, ch: Option<u8>) -> i32 {
        if self.flags & NO_WRITES != 0 {
            self.flags |= ERR_SEEN;
            set_errno(EBADF);
            return EOF;
        }
        if !self.in_put_mode() || !self.has_buf {
            if !self.has_buf {
                self.doallocbuf();
                self.setg(0, 0, 0);
            }
            if self.rp == self.bufsize() {
                self.re = 0;
                self.rp = 0;
            }
            self.wp = self.rp;
            self.wb = self.wp;
            self.we = self.bufsize();
            self.rb = self.re;
            self.rp = self.re;
            self.flags |= CURRENTLY_PUTTING;
            if self.flags & (LINE_BUF | UNBUFFERED) != 0 {
                self.we = self.wp;
            }
        }
        let Some(c) = ch else { return self.do_flush() };
        if self.wp == self.bufsize() && self.do_flush() == EOF {
            return EOF;
        }
        self.buf[self.wp] = c;
        self.wp += 1;
        if (self.flags & UNBUFFERED != 0 || (self.flags & LINE_BUF != 0 && c == b'\n'))
            && self.do_flush() == EOF
        {
            return EOF;
        }
        c as i32
    }

    /// `_IO_default_xsputn`: copy into the free space, overflowing one
    /// character at a time.
    fn default_xsputn(&mut self, data: &[u8]) -> usize {
        let mut i = 0;
        loop {
            if self.wp < self.we {
                let count = (self.we - self.wp).min(data.len() - i);
                self.buf[self.wp..self.wp + count].copy_from_slice(&data[i..i + count]);
                self.wp += count;
                i += count;
            }
            if i == data.len() || self.overflow(Some(data[i])) == EOF {
                break;
            }
            i += 1;
        }
        i
    }

    /// `_IO_new_file_xsputn`: the bytes written, or `None` for glibc's `EOF`
    /// (everything was buffered but a flush failed).
    fn xsputn(&mut self, data: &[u8]) -> Option<usize> {
        let n = data.len();
        if n == 0 {
            return Some(0);
        }
        let mut to_do = n;
        let mut s = 0;
        let mut must_flush = false;
        let mut count = 0;
        if self.flags & LINE_BUF != 0 && self.in_put_mode() {
            count = self.bufsize() - self.wp;
            if count >= n {
                if let Some(p) = data.iter().rposition(|&c| c == b'\n') {
                    count = p + 1;
                    must_flush = true;
                }
            }
        } else if self.we > self.wp {
            count = self.we - self.wp;
        }
        if count > 0 {
            let c = count.min(to_do);
            self.buf[self.wp..self.wp + c].copy_from_slice(&data[..c]);
            self.wp += c;
            s = c;
            to_do -= c;
        }
        if to_do > 0 || must_flush {
            if self.overflow(None) == EOF {
                return if to_do == 0 { None } else { Some(n - to_do) };
            }
            let block = self.bufsize();
            let do_write = to_do - if block >= 128 { to_do % block } else { 0 };
            if do_write > 0 {
                let c = self.new_do_write(0, Some(&data[s..]), do_write);
                to_do -= c;
                if c < do_write {
                    return Some(n - to_do);
                }
            }
            if to_do > 0 {
                to_do -= self.default_xsputn(&data[s + do_write..]);
            }
        }
        Some(n - to_do)
    }

    // ---- reading ----

    /// `_IO_switch_to_get_mode`.
    fn switch_to_get_mode(&mut self) -> i32 {
        if self.wp > self.wb && self.overflow(None) == EOF {
            return EOF;
        }
        self.rb = 0;
        if self.wp > self.re {
            self.re = self.wp;
        }
        self.rp = self.wp;
        self.wb = self.wp;
        self.we = self.wp;
        self.flags &= !CURRENTLY_PUTTING;
        0
    }

    /// `_IO_new_file_underflow`: refill the buffer. Reading a terminal or an
    /// unbuffered stream first writes a line-buffered stdout's pending output.
    fn underflow(&mut self) -> i32 {
        if self.flags & EOF_SEEN != 0 {
            return EOF;
        }
        if self.flags & NO_READS != 0 {
            self.flags |= ERR_SEEN;
            set_errno(EBADF);
            return EOF;
        }
        if self.rp < self.re {
            return self.buf[self.rp] as i32;
        }
        if !self.has_buf {
            self.doallocbuf();
        }
        if self.flags & (LINE_BUF | UNBUFFERED) != 0 {
            super::handle::flush_line_buffered_stdout();
        }
        let _ = self.switch_to_get_mode();
        self.setg(0, 0, 0);
        self.setp(0, 0);
        self.before_read();
        let b = std::mem::take(&mut self.buf);
        let mut b = b;
        let r = self.fd.read(&mut b);
        self.read_done();
        self.buf = b;
        let count = match r {
            Ok(0) => {
                self.flags |= EOF_SEEN;
                0
            }
            Ok(n) => n,
            Err(_) => {
                self.flags |= ERR_SEEN;
                0
            }
        };
        self.re += count;
        if count == 0 {
            self.offset = POS_BAD;
            return EOF;
        }
        if self.offset != POS_BAD {
            self.offset += count as i64;
        }
        self.buf[self.rp] as i32
    }

    /// `__underflow`: leave put mode first.
    fn underflow_generic(&mut self) -> i32 {
        self.used = true;
        if self.in_put_mode() && self.switch_to_get_mode() == EOF {
            return EOF;
        }
        if self.rp < self.re {
            return self.buf[self.rp] as i32;
        }
        self.underflow()
    }

    /// `__uflow` (`getc` on an empty get area): the next byte, or `EOF` (end of
    /// file or error).
    fn uflow(&mut self) -> i32 {
        self.used = true;
        if self.in_put_mode() && self.switch_to_get_mode() == EOF {
            return EOF;
        }
        if self.rp < self.re {
            let c = self.buf[self.rp];
            self.rp += 1;
            return c as i32;
        }
        if self.underflow() == EOF {
            return EOF;
        }
        let c = self.buf[self.rp];
        self.rp += 1;
        c as i32
    }

    /// `_IO_file_xsgetn`: up to `out.wanted()` bytes into `out`, the count
    /// read. Requests of at least a buffer are read directly into `out`, in
    /// whole blocks, after the buffer state is reset; LB-02: pending output is
    /// written first.
    fn xsgetn<D: ReadDest + ?Sized>(&mut self, out: &mut D) -> usize {
        let n = out.wanted();
        let mut got = 0;
        if !self.has_buf {
            self.doallocbuf();
        }
        while got < n {
            let want = n - got;
            let have = self.re - self.rp;
            if want <= have {
                out.put(got, &self.buf[self.rp..self.rp + want]);
                self.rp += want;
                got = n;
                break;
            }
            if have > 0 {
                out.put(got, &self.buf[self.rp..self.re]);
                got += have;
                self.rp += have;
            }
            let want = n - got;
            if self.has_buf && want < self.bufsize() {
                if self.underflow_generic() == EOF {
                    break;
                }
                continue;
            }
            // LB-02: glibc's `_IO_setp` below would forget the pending bytes;
            // `fflush` first (`_IO_new_file_sync`). A failed write ends the
            // read, as a failing `_IO_switch_to_get_mode` ends a small one. A
            // failed seek back over read-ahead (`new_do_write` returns before
            // writing, no error indicator) is no failed write: the bytes are
            // dropped below and the read goes on, as glibc's direct read does
            // (leanrs review F1). Native never makes that `lseek`, so the
            // modelled `errno` it set is put back (review RIO1-17).
            if self.wp > self.wb {
                let saved = errno();
                if self.sync() == EOF {
                    if !self.seek_failed {
                        break;
                    }
                    set_errno(saved);
                }
            }
            self.setg(0, 0, 0);
            self.setp(0, 0);
            let mut count = want;
            let block = self.bufsize();
            if block >= 128 {
                count -= want % block;
            }
            self.before_read();
            let r = out.read(&self.fd, self.regular, got, count);
            self.read_done();
            match r {
                Ok(0) => {
                    self.flags |= EOF_SEEN;
                    break;
                }
                Ok(r) => {
                    got += r;
                    if self.offset != POS_BAD {
                        self.offset += r as i64;
                    }
                }
                Err(_) => {
                    self.flags |= ERR_SEEN;
                    break;
                }
            }
        }
        got
    }

    // ---- positioning ----

    /// `_IO_new_file_sync` (`fflush`): write pending output, give back
    /// read-ahead (seeking back; `ESPIPE` on a pipe is ignored).
    fn sync(&mut self) -> i32 {
        let mut retval = 0;
        if self.wp > self.wb && self.do_flush() != 0 {
            return EOF;
        }
        let delta = self.rp as i64 - self.re as i64;
        if delta != 0 {
            let np = self.sysseek(delta, SEEK_CUR);
            if np != POS_BAD {
                self.re = self.rp;
            } else if errno() != ESPIPE {
                retval = EOF;
            }
        }
        if retval != EOF {
            self.offset = POS_BAD;
        }
        retval
    }

    /// `_IO_new_file_seekoff(fp, offset, dir, _IOS_INPUT|_IOS_OUTPUT)`. A
    /// target inside the buffered bytes is served from the buffer (LB-09: a
    /// second `rewind` returns the buffered bytes, as natively).
    fn seekoff(&mut self, mut offset: i64, mut dir: i32) -> i64 {
        let must_be_exact = self.rb == self.re && self.wb == self.wp;
        let was_writing = self.wp > self.wb || self.in_put_mode();
        if was_writing && self.switch_to_get_mode() != 0 {
            return EOF as i64;
        }
        if !self.has_buf {
            self.doallocbuf();
            self.setp(0, 0);
            self.setg(0, 0, 0);
        }
        let mut dumb = false;
        match dir {
            SEEK_CUR => {
                offset -= self.re as i64 - self.rp as i64;
                if self.offset == POS_BAD {
                    dumb = true;
                } else {
                    offset += self.offset;
                    if offset < 0 {
                        set_errno(EINVAL);
                        return EOF as i64;
                    }
                    dir = SEEK_SET;
                }
            }
            SEEK_END => match self.fd.fstat() {
                Ok(st) if st.st_mode & 0o170000 == 0o100000 => {
                    offset += st.st_size;
                    dir = SEEK_SET;
                }
                _ => dumb = true,
            },
            _ => {}
        }
        if !dumb {
            if self.offset != POS_BAD && self.has_buf {
                let start = self.offset - self.re as i64;
                if offset >= start && offset < self.offset {
                    let (re, p) = (self.re, (offset - start) as usize);
                    self.setg(0, p, re);
                    self.setp(0, 0);
                    self.flags &= !EOF_SEEN;
                    if self.offset >= 0 {
                        self.sysseek(self.offset, SEEK_SET);
                    }
                    return offset;
                }
            }
            if self.flags & NO_READS == 0 {
                let bs = self.bufsize() as i64;
                let mut new_offset = offset & !(bs - 1);
                let mut delta = offset - new_offset;
                if delta > bs {
                    new_offset = offset;
                    delta = 0;
                }
                let result = self.sysseek(new_offset, SEEK_SET);
                if result < 0 {
                    return EOF as i64;
                }
                let mut count: i64 = 0;
                let mut short = false;
                if delta != 0 {
                    let want = if must_be_exact {
                        delta as usize
                    } else {
                        bs as usize
                    };
                    self.before_read();
                    let mut b = std::mem::take(&mut self.buf);
                    count = match self.fd.read(&mut b[..want]) {
                        Ok(c) => c as i64,
                        Err(_) => EOF as i64,
                    };
                    self.read_done();
                    self.buf = b;
                    if count < delta {
                        offset = if count == EOF as i64 {
                            delta
                        } else {
                            delta - count
                        };
                        dir = SEEK_CUR;
                        short = true;
                    }
                }
                if !short {
                    self.setg(0, delta as usize, count as usize);
                    self.setp(0, 0);
                    self.offset = result + count;
                    self.flags &= !EOF_SEEN;
                    return offset;
                }
            }
        }
        // dumb:
        let result = self.sysseek(offset, dir);
        if result != EOF as i64 {
            self.flags &= !EOF_SEEN;
            self.offset = result;
            self.setg(0, 0, 0);
            self.setp(0, 0);
        }
        result
    }

    /// `ftello` (`do_ftell`): the stream's position, counting pending output
    /// and read-ahead; `-1` with `errno` set on failure.
    pub fn ftell(&mut self) -> i64 {
        let mut adj: i64 = 0;
        if self.has_buf {
            let unflushed = self.wp > self.wb;
            if unflushed && self.flags & IS_APPENDING != 0 {
                let r = self.sysseek(0, SEEK_END);
                if r == POS_BAD {
                    return EOF as i64;
                }
                self.offset = r;
            }
            if !unflushed {
                adj -= self.re as i64 - self.rp as i64;
            } else if self.flags & IS_APPENDING != 0 {
                // glibc: in append mode the end of the read area is not the
                // file offset; the write base is (review RIO1-09)
                adj += self.wp as i64 - self.wb as i64;
            } else {
                adj += self.wp as i64 - self.re as i64;
            }
        }
        let r = if self.offset != POS_BAD {
            self.offset
        } else {
            self.sysseek(0, SEEK_CUR)
        };
        if r == EOF as i64 {
            return r;
        }
        let r = r + adj;
        if r < 0 {
            set_errno(EINVAL);
            return EOF as i64;
        }
        r
    }

    // ---- Lean's operations (`lean_io_prim_handle_*`) ----

    /// `Handle.putStr` and `Handle.write`: `fwrite`; `Err(errno)` unless every
    /// byte was taken. No bytes succeed at once.
    pub fn put(&mut self, data: &[u8]) -> Result<(), i32> {
        if data.is_empty() {
            return Ok(());
        }
        self.used = true;
        match self.xsputn(data) {
            None => Ok(()),
            Some(m) if m == data.len() => Ok(()),
            Some(_) => Err(errno()),
        }
    }

    /// `Handle.flush`: `fflush`.
    pub fn flush(&mut self) -> Result<(), i32> {
        if self.sync() == 0 {
            Ok(())
        } else {
            Err(errno())
        }
    }

    /// `Handle.read`: `fread` into `out` (Lean's array of `n = out.len()`
    /// bytes, which the caller allocated); the count read. Any bytes read are
    /// a success; with none, end of file clears both indicators (`clearerr`)
    /// and gives 0, otherwise it is `Err(errno)`. `n = 0` touches nothing.
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, i32> {
        self.fread(out)
    }

    /// [`CFile::read`] into uninitialized memory (a translator's new
    /// `ByteArray`, allocated as Lean allocates it, not zeroed): the count
    /// read; `out[..count]` is initialized, the rest untouched.
    pub fn read_uninit(&mut self, out: &mut [MaybeUninit<u8>]) -> Result<usize, i32> {
        self.fread(out)
    }

    /// [`CFile::read`] of up to `n` bytes appended to `out`, which grows with
    /// the bytes read (no room is reserved for `n`); the count read. Copies
    /// out of the stream's buffer append. With `want = n - have` still wanted
    /// after the `have` buffered bytes, a direct read happens when `want >=
    /// bufsize`, of `want - want % bufsize` bytes: through std's `read_to_end`
    /// from a regular file the stream owns, with no zero pass; otherwise into
    /// zeroed windows of at most a pipe's capacity or 64 KiB (`sys::VecDest`;
    /// the module comment of `lean_runtime::io` has the rule and an example).
    pub fn read_vec(&mut self, n: usize, out: &mut Vec<u8>) -> Result<usize, i32> {
        let start = out.len();
        self.fread(&mut VecDest { v: out, start, n })
    }

    fn fread<D: ReadDest + ?Sized>(&mut self, out: &mut D) -> Result<usize, i32> {
        if out.wanted() == 0 {
            return Ok(0);
        }
        self.used = true;
        let got = self.xsgetn(out);
        if got > 0 {
            Ok(got)
        } else if self.is_eof() {
            self.clearerr();
            Ok(0)
        } else {
            Err(errno())
        }
    }

    /// `Handle.getLine`: `getc` up to and including `\n` (or to end of file or
    /// an error), each byte appended to `out`; then an error of this call is
    /// `Err(errno)`, and the caller drops the bytes appended (Lean loses the
    /// line); otherwise end of file is cleared.
    ///
    /// **LB-41** (`docs/lean-bugs.md`): the error indicator is cleared first,
    /// so only this call's error is reported. Lean's `getLine` tests
    /// `ferror` after its loop without clearing it before (io.cpp 659-660),
    /// and the indicator stays set until end of file (`clearerr` at 662): one
    /// failure (an `EAGAIN` of a non-blocking descriptor, a failed write on
    /// the handle) makes every later `getLine` read its line and fail with
    /// whatever `errno` holds, the line lost. Only `getLine` reads the
    /// indicator, so `read`, `putStr`, `flush` and end of file stay as
    /// natively.
    ///
    /// **A sink that stops** ([`ByteSink::stopped`]; AR-19): `get_line` asks
    /// the sink before each read of the descriptor and once the line is
    /// complete. Once the sink has stopped, `get_line` reads no further and
    /// returns `Err(ENOMEM)`; the bytes it took from the stream stay consumed
    /// (the sink dropped them), and the end-of-file indicator and the
    /// modelled `errno` are left as they were (the error indicator is
    /// cleared, as at every call). The glue, whose sink stopped, then ends the
    /// process with its out-of-memory report. Natively
    /// `std::string::push_back` throws `std::bad_alloc`, which nothing
    /// catches: libc++ prints `terminating due to uncaught exception of type
    /// std::bad_alloc` and aborts (status 134). With a sink that never stops
    /// (the default), the bytes and the system calls are those of before.
    pub fn get_line<S: ByteSink + ?Sized>(&mut self, out: &mut S) -> Result<(), i32> {
        self.flags &= !ERR_SEEN;
        loop {
            // The bytes in the get area, up to a newline, in one copy.
            if self.rp < self.re {
                let avail = &self.buf[self.rp..self.re];
                match avail.iter().position(|&b| b == b'\n') {
                    Some(k) => {
                        out.extend_from_slice(&avail[..=k]);
                        self.rp += k + 1;
                        break;
                    }
                    None => {
                        out.extend_from_slice(avail);
                        self.rp = self.re;
                    }
                }
            }
            // A line without end (stdin from `/dev/zero`) is not read on
            // into a sink that drops it.
            if out.stopped() {
                return Err(ENOMEM);
            }
            let c = self.uflow();
            if c == EOF {
                break;
            }
            out.extend_from_slice(&[c as u8]);
            if c == b'\n' as i32 {
                break;
            }
        }
        // the sink may have dropped the line's last bytes
        if out.stopped() {
            return Err(ENOMEM);
        }
        if self.flags & ERR_SEEN != 0 {
            return Err(errno());
        }
        if self.is_eof() {
            self.clearerr();
        }
        Ok(())
    }

    /// `Handle.rewind`: `fseek(fp, 0, SEEK_SET)`.
    pub fn rewind(&mut self) -> Result<(), i32> {
        if self.seekoff(0, SEEK_SET) == EOF as i64 {
            Err(errno())
        } else {
            Ok(())
        }
    }

    /// `Handle.truncate`: `ftruncate(fileno(fp), ftello(fp))`, without
    /// flushing (a failed `ftello` is `ftruncate(fd, -1)`, `EINVAL`).
    pub fn truncate(&mut self) -> Result<(), i32> {
        let pos = self.ftell();
        self.fd.ftruncate(pos)
    }

    /// `isatty(fileno(fp))` (`Handle.isTty`; a failure sets `errno`).
    pub fn is_tty(&self) -> bool {
        self.fd.isatty()
    }

    /// `flock(fileno(fp), op)`. It may block, and the caller holds the
    /// stream meanwhile; `Handle::lock` & co. wait on `CFile::descriptor`'s
    /// clone with the stream unlocked instead, as native takes no `FILE` lock.
    pub fn flock(&self, op: rustix::fs::FlockOperation) -> Result<(), i32> {
        self.fd.flock(op)
    }

    /// The descriptor, for a call that may block and needs no stream state
    /// (`flock(fileno(fp), op)`): it stays open while the clone lives, and
    /// the caller drops the stream's lock before the call.
    pub(crate) fn descriptor(&self) -> Fd {
        self.fd.clone()
    }

    /// `fclose`: write pending output and drop this stream's hold on the
    /// descriptor (errors ignored, as Lean's handle finalizer ignores them).
    /// The descriptor closes when its last holder goes: at once for a stream
    /// made by [`CFile::fdopen`] alone, but for a handle's stream only when the
    /// handle's file drops (it keeps a shared clone for `flock`); a standard
    /// stream's descriptor is never closed. While `exit::force_exit` ends the
    /// process (`_Exit`), the pending output is discarded instead (leanrs
    /// review F3). Crate-internal (review RIO1-11):
    /// closing a handle's stream through `Handle::file()` would leave its
    /// descriptor open; a stream closes when it is dropped.
    pub(crate) fn close(&mut self) {
        if matches!(self.fd, Fd::Closed) {
            return;
        }
        if self.flags & NO_WRITES == 0
            && self.in_put_mode()
            && !super::exit::exiting_without_flush()
        {
            let _ = self.do_flush();
        }
        self.fd = Fd::Closed;
        self.buf = Vec::new();
        self.has_buf = false;
    }

    /// The pending output and the descriptor, after a `flush_nowait` that
    /// returned false (its compaction put the rest at the buffer's start),
    /// for the writer thread that finishes the close (`io::coop::hand_off`);
    /// the stream is left closed, with nothing pending.
    #[cfg(feature = "sched")]
    pub(crate) fn hand_off(&mut self) -> (Vec<u8>, Fd) {
        let mut bytes = std::mem::take(&mut self.buf);
        bytes.truncate(self.wp);
        bytes.drain(..self.wb);
        self.has_buf = false;
        self.setg(0, 0, 0);
        self.wb = 0;
        self.wp = 0;
        self.we = 0;
        (bytes, std::mem::replace(&mut self.fd, Fd::Closed))
    }

    /// In a no-suspend scope (the drop of a handle's last reference): write
    /// as much of the pending output as the descriptor takes without
    /// blocking (`io::coop::write_nowait`). True when nothing is left pending
    /// (or the descriptor never blocks, and `close` writes it as usual);
    /// false when the descriptor would block with bytes still pending, which
    /// stay in the put area for a later flush (review RSIO-09). A write error
    /// loses the rest and sets the error indicator, as `new_do_write` does.
    #[cfg(feature = "sched")]
    pub(crate) fn flush_nowait(&mut self) -> bool {
        if matches!(self.fd, Fd::Closed)
            || self.flags & NO_WRITES != 0
            || !self.in_put_mode()
            || self.wp <= self.wb
            || super::exit::exiting_without_flush()
        {
            return true;
        }
        // Read-ahead to give back first (a read-write stream): the usual
        // path, which seeks.
        if self.flags & IS_APPENDING == 0 && self.re != self.wb {
            return true;
        }
        while self.wb < self.wp {
            let r =
                super::coop::write_nowait(&self.fd, &self.buf[self.wb..self.wp], &mut self.coop);
            match r {
                None => return true,
                Some(Ok(n)) => {
                    self.wb += n;
                    if self.offset >= 0 {
                        self.offset += n as i64;
                    }
                }
                Some(Err(EAGAIN)) => {
                    // What is left goes to the start of the buffer, with no
                    // read-ahead, so the later flush writes it from there
                    // instead of seeking back over a moved write base, which
                    // a pipe refuses (`ESPIPE`) and the bytes would be lost
                    // (review RSIO-12).
                    let left = self.wp - self.wb;
                    self.buf.copy_within(self.wb..self.wp, 0);
                    self.setg(0, 0, 0);
                    self.wb = 0;
                    self.wp = left;
                    if self.flags & (LINE_BUF | UNBUFFERED) != 0 {
                        self.we = self.wp;
                    }
                    return false;
                }
                Some(Err(_)) => {
                    self.flags |= ERR_SEEN;
                    break;
                }
            }
        }
        self.setg(0, 0, 0);
        self.wb = 0;
        self.wp = 0;
        self.we = if self.flags & (LINE_BUF | UNBUFFERED) != 0 {
            0
        } else {
            self.bufsize()
        };
        true
    }

    /// `_IO_OVERFLOW(fp, EOF)`: write pending output (entering put mode).
    pub(crate) fn flush_pending(&mut self) -> i32 {
        self.overflow(None)
    }

    /// One stream's step of `_IO_flush_all` at exit: write pending output.
    pub fn exit_flush(&mut self) {
        if !matches!(self.fd, Fd::Closed) && self.wp > self.wb {
            let _ = self.overflow(None);
        }
    }

    /// One stream's step of `_IO_unbuffer_all` at exit: `setbuf(fp, NULL)`
    /// of a used, buffered stream syncs it, which gives seekable read-ahead
    /// back (the next process reads stdin from where the program stopped).
    pub fn exit_unbuffer(&mut self) {
        if !matches!(self.fd, Fd::Closed) && self.flags & UNBUFFERED == 0 && self.used {
            let _ = self.sync();
        }
    }
}

#[cfg(test)]
#[path = "cfile_tests.rs"]
mod tests;

impl Drop for CFile {
    /// A stream that goes away is closed as `fclose` closes it.
    fn drop(&mut self) {
        self.close();
    }
}
