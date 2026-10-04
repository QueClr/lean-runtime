//! The system calls the FILE model and the handles make, over rustix's safe
//! API. Each failing call sets the crate's modelled `errno`
//! ([`super::error::set_errno`]) as the C call would, and returns the code.
//!
//! Replaces lean2rr's raw `extern "C"` declarations of `read`, `write`,
//! `lseek`, `ftruncate`, `isatty`, `flock` and `open` (leanrt `cfile.rs`,
//! `fs.rs`).

use super::error::{set_errno, EINVAL};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};

/// A stream's file descriptor: one of the standard ones (never closed by the
/// crate), one a handle owns (closed with it), or none (after `fclose`).
#[derive(Debug)]
pub(crate) enum Fd {
    Std(u8),
    Owned(OwnedFd),
    Closed,
}

/// `rustix::io::Errno` as the `errno` value, recorded in the model.
fn fail(e: rustix::io::Errno) -> i32 {
    let code = e.raw_os_error();
    set_errno(code);
    code
}

impl Fd {
    /// The descriptor, borrowed; `None` once closed.
    pub(crate) fn borrow(&self) -> Option<BorrowedFd<'_>> {
        match self {
            Fd::Std(0) => Some(rustix::stdio::stdin()),
            Fd::Std(1) => Some(rustix::stdio::stdout()),
            Fd::Std(_) => Some(rustix::stdio::stderr()),
            Fd::Owned(f) => Some(f.as_fd()),
            Fd::Closed => None,
        }
    }

    /// `fileno`: the descriptor's number, `-1` once closed.
    pub(crate) fn raw(&self) -> i32 {
        use rustix::fd::AsRawFd;
        match self.borrow() {
            Some(f) => f.as_raw_fd(),
            None => -1,
        }
    }

    fn get(&self) -> Result<BorrowedFd<'_>, i32> {
        self.borrow().ok_or_else(|| fail(rustix::io::Errno::BADF))
    }

    /// `read(2)` into `buf`.
    pub(crate) fn read(&self, buf: &mut [u8]) -> Result<usize, i32> {
        rustix::io::read(self.get()?, buf).map_err(fail)
    }

    /// `write(2)` of `buf`.
    pub(crate) fn write(&self, buf: &[u8]) -> Result<usize, i32> {
        rustix::io::write(self.get()?, buf).map_err(fail)
    }

    /// `lseek(2)`; `whence` is 0 (`SEEK_SET`), 1 (`SEEK_CUR`) or 2
    /// (`SEEK_END`). A negative absolute offset is the kernel's `EINVAL`.
    pub(crate) fn lseek(&self, off: i64, whence: i32) -> Result<i64, i32> {
        use rustix::fs::SeekFrom;
        let fd = self.get()?;
        let pos = match whence {
            0 if off < 0 => return Err(fail(rustix::io::Errno::INVAL)),
            0 => SeekFrom::Start(off as u64),
            1 => SeekFrom::Current(off),
            _ => SeekFrom::End(off),
        };
        rustix::fs::seek(fd, pos).map(|p| p as i64).map_err(fail)
    }

    /// `ftruncate(2)`; a negative length is the kernel's `EINVAL`.
    pub(crate) fn ftruncate(&self, len: i64) -> Result<(), i32> {
        let fd = self.get()?;
        if len < 0 {
            set_errno(EINVAL);
            return Err(EINVAL);
        }
        rustix::fs::ftruncate(fd, len as u64).map_err(fail)
    }

    /// `fstat(2)`.
    pub(crate) fn fstat(&self) -> Result<rustix::fs::Stat, i32> {
        rustix::fs::fstat(self.get()?).map_err(fail)
    }

    /// glibc's `isatty` (`tcgetattr`): `false` with `errno` set (`ENOTTY`,
    /// `EBADF`) when the descriptor is not a terminal.
    pub(crate) fn isatty(&self) -> bool {
        match self.get() {
            Ok(fd) => rustix::termios::tcgetattr(fd).map_err(fail).is_ok(),
            Err(_) => false,
        }
    }

    /// `isatty` that leaves `errno` alone (glibc's `local_isatty` in
    /// `_IO_file_doallocate`).
    pub(crate) fn isatty_keep_errno(&self) -> bool {
        self.borrow()
            .is_some_and(|fd| rustix::termios::tcgetattr(fd).is_ok())
    }

    /// `flock(2)`.
    pub(crate) fn flock(&self, op: rustix::fs::FlockOperation) -> Result<(), i32> {
        rustix::fs::flock(self.get()?, op).map_err(fail)
    }
}

/// `open(2)` with `flags` and mode `0666` (`lean_io_prim_handle_mk`); `path`
/// holds no NUL byte.
pub(crate) fn open(path: &[u8], flags: rustix::fs::OFlags) -> Result<OwnedFd, i32> {
    let p =
        std::path::Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path));
    rustix::fs::open(p, flags, rustix::fs::Mode::from_raw_mode(0o666)).map_err(fail)
}
