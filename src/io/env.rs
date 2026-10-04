//! The process rows: `IO.getEnv`, `IO.appPath`, `IO.Process.getPID`,
//! `IO.getRandomBytes`, the monotonic clock and `IO.sleep`.
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/env.rs` (`get_env`, `app_path`,
//! `get_random_bytes`, `sleep`), lean2rr's `runtime/leanrt/src/fs.rs`
//! (`get_random_bytes`: `EINTR` retried, the descriptor kept open on `ENOMEM`)
//! and `io.rs` (`mono_nanos`: Lean's absolute `CLOCK_MONOTONIC`, owner's
//! ruling DV10). The environment copy that `uv_os_environ` and child
//! processes need belongs to the second io batch.

use super::error::{set_errno, IoError, EINTR, ENOMEM};
use super::handle::sarray_would_overflow;
use super::ByteSink;
use std::mem::MaybeUninit;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// `IO.getEnv` (`lean_io_getenv`, glibc's `getenv`): a name holding a NUL
/// byte or empty has no value; otherwise the value of the first entry
/// `name=value` of the environment is appended to `out` and `true` returned.
/// As glibc's `getenv`, a name holding `=` matches the start of an entry
/// (`A=x` finds `A=x=v` and gives `v`). The caller decodes the bytes as
/// Lean's `mk_string` does (lossily).
pub fn get_env<S: ByteSink + ?Sized>(name: &[u8], out: &mut S) -> bool {
    if name.is_empty() || name.contains(&0) {
        return false;
    }
    if !name.contains(&b'=') {
        return match std::env::var_os(std::ffi::OsStr::from_bytes(name)) {
            Some(v) => {
                out.extend_from_slice(v.as_bytes());
                true
            }
            None => false,
        };
    }
    for (k, v) in std::env::vars_os() {
        let mut entry = k.into_vec();
        entry.push(b'=');
        entry.extend_from_slice(v.as_bytes());
        if entry.len() > name.len() && entry.starts_with(name) && entry[name.len()] == b'=' {
            out.extend_from_slice(&entry[name.len() + 1..]);
            return true;
        }
    }
    false
}

/// The size of the buffers Lean's C code passes to path functions.
pub(crate) const PATH_MAX: usize = 4096;

/// `IO.appPath` (`lean_io_app_path`): `readlink("/proc/<pid>/exe")` into a
/// `PATH_MAX - 1` buffer (a longer target is cut there), appended to `out`;
/// a failure is Lean's user error `failed to locate application`.
pub fn app_path<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    match std::fs::read_link("/proc/self/exe") {
        Ok(p) => {
            let b = p.as_os_str().as_bytes();
            out.extend_from_slice(&b[..b.len().min(PATH_MAX - 1)]);
            Ok(())
        }
        Err(e) => {
            set_errno(e.raw_os_error().unwrap_or(0));
            Err(IoError::user_error("failed to locate application"))
        }
    }
}

/// `IO.Process.getPID` (`lean_io_process_get_pid`, `getpid`).
pub fn get_pid() -> u32 {
    nix::unistd::getpid().as_raw() as u32
}

/// `/dev/urandom`, open for one `IO.getRandomBytes` call: what
/// `lean_io_get_random_bytes` does before it allocates its array, so the
/// caller allocates after the open, as Lean does (AR-1).
///
/// ```text
/// let src = open_random(n)?;      // the open's error names /dev/urandom
/// let mut a = allocate n bytes;   // the translator's ByteArray
/// src.fill(&mut a)?;              // or fill_uninit
/// ```
///
/// So with no descriptor left (`EMFILE`) or no `/dev`, even an `n` too big
/// to allocate fails with the open's catchable error, as natively, instead
/// of ending in the allocation's out of memory. The descriptor closes when
/// the source is filled or dropped.
#[derive(Debug)]
pub struct RandomSource {
    /// `None` for `n = 0`, for which Lean opens nothing.
    file: Option<std::fs::File>,
}

/// The steps of `lean_io_get_random_bytes` before its array exists
/// (io.cpp 866-877): `n = 0` opens nothing (an empty source); otherwise
/// `/dev/urandom` is opened with `O_RDONLY | O_CLOEXEC`, an error naming it,
/// and an `n` whose array would overflow (Lean's 24-byte header) fails with
/// `ENOMEM`, the descriptor left open as Lean leaves it. The caller then
/// allocates `n` bytes and fills them ([`RandomSource::fill`]).
pub fn open_random(n: usize) -> Result<RandomSource, IoError> {
    if n == 0 {
        return Ok(RandomSource { file: None });
    }
    let f = open_urandom()?;
    if sarray_would_overflow(n) {
        std::mem::forget(f);
        return Err(IoError::decode_io_error(ENOMEM, None));
    }
    Ok(RandomSource { file: Some(f) })
}

impl RandomSource {
    /// The read loop of `lean_io_get_random_bytes` (io.cpp 882-917) into
    /// `out`, the `n` bytes allocated after [`open_random`]: `read` until
    /// `out` is full, `EINTR` retried, another error reported without a file
    /// name; then the descriptor closes. A source opened for `n = 0` given a
    /// non-empty `out` opens `/dev/urandom` then.
    pub fn fill(self, out: &mut [u8]) -> Result<(), IoError> {
        self.fill_with(out.len(), |f, done| rustix::io::read(f, &mut out[done..]))
    }

    /// [`RandomSource::fill`] into uninitialized memory (the translator's new
    /// `ByteArray`, not zeroed); on `Ok` all of `out` is initialized.
    pub fn fill_uninit(self, out: &mut [MaybeUninit<u8>]) -> Result<(), IoError> {
        self.fill_with(out.len(), |f, done| {
            rustix::io::read(f, &mut out[done..]).map(|(init, _)| init.len())
        })
    }

    fn fill_with(
        self,
        n: usize,
        read: impl FnMut(&std::fs::File, usize) -> rustix::io::Result<usize>,
    ) -> Result<(), IoError> {
        if n == 0 {
            return Ok(());
        }
        let f = match self.file {
            Some(f) => f,
            None => open_urandom()?,
        };
        read_random(&f, n, read)
    }
}

/// `IO.getRandomBytes`'s checks before its array exists
/// (`lean_io_get_random_bytes`): for `n > 0` whose array would overflow,
/// Lean opens `/dev/urandom` first (an open error names it), then fails with
/// `ENOMEM` and leaves the descriptor open, as here. A translator calls it,
/// allocates `n` bytes, then calls [`get_random_bytes`] on them. That
/// opens `/dev/urandom` after the allocation; [`open_random`] keeps Lean's
/// order.
pub fn check_random_size(n: usize) -> Result<(), IoError> {
    if n == 0 || !sarray_would_overflow(n) {
        return Ok(());
    }
    let f = open_urandom()?;
    std::mem::forget(f);
    Err(IoError::decode_io_error(ENOMEM, None))
}

fn open_urandom() -> Result<std::fs::File, IoError> {
    // a descriptor is allocated: the context's handed-off streams end first
    super::effect_point();
    // std opens with O_RDONLY | O_CLOEXEC, as Lean does
    std::fs::File::open("/dev/urandom").map_err(|e| {
        let code = e.raw_os_error().unwrap_or(0);
        set_errno(code);
        IoError::decode_io_error(code, Some(b"/dev/urandom"))
    })
}

/// `IO.getRandomBytes` (`lean_io_get_random_bytes`) into `out`, the `n`
/// bytes the caller allocated after [`check_random_size`]: `n = 0` opens
/// nothing; otherwise `/dev/urandom` is opened (an error names it) and read
/// until `out` is full, `EINTR` retried, another error reported without a
/// file name.
pub fn get_random_bytes(out: &mut [u8]) -> Result<(), IoError> {
    fill_random(out.len(), |f, done| rustix::io::read(f, &mut out[done..]))
}

/// [`get_random_bytes`] into uninitialized memory (the translator's new
/// `ByteArray`, not zeroed); on `Ok` all of `out` is initialized.
pub fn get_random_bytes_uninit(out: &mut [MaybeUninit<u8>]) -> Result<(), IoError> {
    fill_random(out.len(), |f, done| {
        rustix::io::read(f, &mut out[done..]).map(|(init, _)| init.len())
    })
}

/// `/dev/urandom` opened, then read into `n` bytes (see [`read_random`]).
fn fill_random(
    n: usize,
    read: impl FnMut(&std::fs::File, usize) -> rustix::io::Result<usize>,
) -> Result<(), IoError> {
    if n == 0 {
        return Ok(());
    }
    let f = open_urandom()?;
    read_random(&f, n, read)
}

/// The loop of `lean_io_get_random_bytes`: `read` from `done` on until `n`
/// bytes are in.
fn read_random(
    f: &std::fs::File,
    n: usize,
    mut read: impl FnMut(&std::fs::File, usize) -> rustix::io::Result<usize>,
) -> Result<(), IoError> {
    let mut done = 0;
    while done < n {
        match read(f, done) {
            Ok(got) => done += got,
            Err(e) => {
                let code = e.raw_os_error();
                if code != EINTR {
                    set_errno(code);
                    return Err(IoError::decode_io_error(code, None));
                }
            }
        }
    }
    Ok(())
}

/// `IO.monoNanosNow` (`lean_io_mono_nanos_now`): `std::chrono::steady_clock`,
/// that is `CLOCK_MONOTONIC`, in nanoseconds since its (boot-time) epoch, as
/// Lean counts it (DV10).
#[inline]
pub fn mono_nanos_now() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    (t.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(t.tv_nsec as u64)
}

/// `IO.monoMsNow` (`lean_io_mono_ms_now`): the same clock in whole
/// milliseconds.
#[inline]
pub fn mono_ms_now() -> u64 {
    mono_nanos_now() / 1_000_000
}

/// `IO.sleep` (Lean code over `dbgSleep`, `std::this_thread::sleep_for`): `ms`
/// milliseconds.
pub fn sleep(ms: u32) {
    std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
}
