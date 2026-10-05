//! The startup descriptors in an ELF constructor of the crate: native Lean
//! has libuv's loop descriptors open before `main` (epoll, the io_uring
//! rings, the signal lock pipe, the signal pipe, the eventfd:
//! [`super::startup`]), and this file opens them before Rust's runtime
//! starts, so no translator writes a constructor of its own (audit item 4.3:
//! lean2rr's `rt.rs` had one).
//!
//! This file holds one of the crate's `unsafe` items, a native quirk that no
//! safe API can reproduce: `UNSAFE.md` has its entry, and
//! `docs/native-quirks.md` ("The startup descriptors in a constructor") its
//! invariants and proof. It is compiled only with the feature `startup-fds`;
//! without it, a glue that wants native's descriptors writes the constructor
//! itself ([`super::startup::open_native_descriptors`]), and the crate root
//! forbids `unsafe` unless another such feature is on.
//!
//! **Why a constructor.** Rust's runtime, before `main`
//! (`sanitize_standard_fds` in `lang_start`), opens `/dev/null` read-write in
//! the place of each closed standard descriptor. Natively libuv's loop opens
//! at the lowest free numbers, so a closed standard descriptor is taken by
//! the epoll descriptor (or the next one), and reading a closed stdin then
//! fails with `EINVAL` (case `io/startup_closed_stdio`). Opened before
//! `lang_start`, the descriptors take those numbers too; opened after, they
//! could only take them back from a `/dev/null` that may be one the program
//! was given (`<>/dev/null`, Python's `subprocess.DEVNULL`).
//!
//! **What it does.**
//! - The constructor ([`constructor`], section `.init_array.00101`) first
//!   checks that it is in the program's own executable (its address inside
//!   the kernel's `start_code` to `end_code`, `/proc/self/stat` read into a
//!   stack buffer, as `argv_title`'s check): in a shared library, or when
//!   that cannot be told, it does nothing, so a library never opens
//!   descriptors in its host nor ends it (review RSH2-02). Then it reads
//!   `UV_USE_IO_URING` with glibc's `getenv` (U1: `std::env::var_os` would
//!   allocate a copy) and calls `open_native_descriptors_with`, which
//!   allocates nothing. On failure it reads `LEAN_ABORT_ON_PANIC` the same
//!   way and ends the process as [`super::startup::end_startup`] does (the
//!   line from a stack buffer). It never allocates (AR-36), installs no
//!   signal handler (Lean's stack-overflow report is installed after Rust's
//!   runtime start: review SO-2) and starts no thread of the process (the
//!   kernel starts the polling ring's thread, which runs no code of the
//!   process).
//! - [`ensure_native_descriptors`], which the glue calls at `main`'s start,
//!   refers to the constructor (so the linker keeps the object that holds
//!   it) and does nothing more when the descriptors are open. If they are
//!   not (the constructor did not act), it opens them where they land: a
//!   standard descriptor closed at startup then stays Rust's `/dev/null`,
//!   and the descriptors' numbers are not native's (it closes nothing:
//!   reviews RSH2-04, LS2-01).
//!
//! **When it runs** (AR-20). The linkers put the `.init_array.N` sections
//! first, by increasing N, then the plain ones; glibc calls them in that
//! order. So it runs after the toolchain's constructors (90, 99) and
//! `proc-title`'s (100: `lean_setup_args` runs before libuv's loop opens,
//! natively, and that constructor needs two free descriptors under
//! `ulimit -n 12`), and before the program's own (a priority above 101, or
//! none). Shared libraries' constructors still run before it.
//!
//! Source: lean2rr's `runtime/leanrt/src/rt.rs` (`startup_descriptors`,
//! `reserve_native_descriptors`), with a priority (lean2rr's was plain
//! `.init_array`), the executable check, and without lean2rr's recovery,
//! which closed the read-write `/dev/null`s on descriptors 0 to 2 when the
//! constructor had not run: a safe function that closes descriptors it does
//! not own rests on a caller contract (reviews RSH2-04, LS2-01).

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::ffi::CStr;

/// The value of the environment variable `name`, as glibc's `getenv` finds
/// it, given to `f` (`None` when unset). No allocation: the bytes are read
/// in place, in `environ`, while `f` runs.
fn with_env<R>(name: &CStr, f: impl FnOnce(Option<&[u8]>) -> R) -> R {
    // SAFETY: (U1) `name` is a NUL-terminated string. glibc's `getenv` reads
    // `environ` and returns null or a pointer into one of its entries, after
    // the `=`. Only the crate's constructor calls this, before `main`, while
    // no code changes the environment (G3), so the read races with no write.
    let p = unsafe { nix::libc::getenv(name.as_ptr()) };
    if p.is_null() {
        return f(None);
    }
    // SAFETY: (U1) `p` points to the NUL-terminated rest of an entry of
    // `environ`, which stays as it is while `f` runs: no code changes the
    // environment meanwhile (G3), and the slice does not outlive this call.
    let value = unsafe { CStr::from_ptr(p) }.to_bytes();
    f(Some(value))
}

/// What the constructor does, `code` being its address: in the program's
/// own executable, open the descriptors, or end the process as natively the
/// loop's failure does (`end_startup`); elsewhere nothing. Allocates
/// nothing.
fn open_at_startup(code: usize) {
    // a shared library (also one loaded by `dlopen`), or no `/proc`: do
    // nothing, so a library never opens descriptors in its host nor ends it
    // (review RSH2-02); `ensure_native_descriptors` opens them in `main`
    if super::proc_stat::code_in_main_executable(code) != Some(true) {
        return;
    }
    // the lib's own unit tests: one that checks `ensure_native_descriptors`'s
    // fallback runs in a child process whose constructor opens nothing
    #[cfg(test)]
    if with_env(c"LEAN_RUNTIME_TEST_NO_STARTUP_FDS", |v| v.is_some()) {
        return;
    }
    let opened = with_env(
        c"UV_USE_IO_URING",
        super::startup::open_native_descriptors_with,
    );
    if let Err(f) = opened {
        with_env(c"LEAN_ABORT_ON_PANIC", |abort| {
            super::startup::end_startup_with(f, abort)
        })
    }
}

/// The ELF constructor: glibc calls each `.init_array` function before
/// `main` (with `argc`, `argv` and `envp`, unused here). glibc only, as the
/// crate's `io`; not under Miri, which runs no constructor of this kind and
/// no system call that opens these descriptors.
#[cfg(all(target_os = "linux", target_env = "gnu", not(miri)))]
mod constructor {
    use std::ffi::{c_char, c_int};

    extern "C" fn open_startup_descriptors(
        _argc: c_int,
        _argv: *mut *mut c_char,
        _envp: *mut *mut c_char,
    ) {
        // nothing here unwinds (an unwind could not leave an `extern "C"`
        // function anyway: Rust aborts)
        super::open_at_startup((open_startup_descriptors as *const ()).addr())
    }

    /// Referred to by [`super::keep_constructor`]. Priority 101: after the
    /// toolchain's constructors (std's own `argv` record is 99) and
    /// `proc-title`'s (100), before the program's (AR-20).
    #[used]
    #[link_section = ".init_array.00101"]
    pub(super) static OPEN_STARTUP_DESCRIPTORS: extern "C" fn(
        c_int,
        *mut *mut c_char,
        *mut *mut c_char,
    ) = open_startup_descriptors;
}

/// Refers to the constructor, so that the linker keeps the object that holds
/// it wherever [`ensure_native_descriptors`], `open_native_descriptors` or
/// `mark_end_initialization` is linked (a `#[used]` static alone does not
/// keep its object file, from an rlib or an archive).
#[inline]
pub(crate) fn keep_constructor() {
    #[cfg(all(target_os = "linux", target_env = "gnu", not(miri)))]
    std::hint::black_box(&constructor::OPEN_STARTUP_DESCRIPTORS);
}

/// See [`super::startup::ensure_native_descriptors`] (re-exported there).
pub fn ensure_native_descriptors() {
    keep_constructor();
    if super::startup::descriptors_opened() {
        return;
    }
    if let Err(f) = super::startup::open_native_descriptors() {
        super::startup::end_startup(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constructor ran in this test binary (the lib's unit tests link
    /// it as any binary does, in the executable): the descriptors are open.
    #[test]
    #[cfg_attr(miri, ignore)] // no constructor under Miri
    fn the_constructor_opened_them() {
        assert!(crate::io::startup::descriptors_opened());
        ensure_native_descriptors();
    }

    /// The fallback when the constructor did not act (a child process whose
    /// constructor opens nothing, started with its standard input closed):
    /// `ensure_native_descriptors` closes nothing, so descriptor 0 stays the
    /// `/dev/null` Rust's runtime put there, and the descriptors open where
    /// they land, the epoll descriptor at 3 (natively it takes 0: case
    /// `io/startup_closed_stdio`; reviews RSH2-04, LS2-01).
    #[test]
    #[cfg_attr(miri, ignore)] // Miri runs no process
    fn without_the_constructor_they_open_where_they_land() {
        const CHILD: &str = "LEAN_RUNTIME_TEST_NO_STARTUP_FDS";
        if std::env::var_os(CHILD).is_none() {
            let exe = std::env::current_exe().unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                // descriptors 3 to 9 a runner left open (a lock's, a
                // jobserver's) closed, so the child starts with 1 and 2
                // only, as a program does
                .arg("exec 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-; exec \"$0\" \"$@\" 0<&-")
                .arg(exe)
                .args([
                    "--exact",
                    "io::startup_fds::tests::without_the_constructor_they_open_where_they_land",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("1 passed"),
                "{out:?}"
            );
            return;
        }
        let link = |fd: u32| {
            std::fs::read_link(format!("/proc/self/fd/{fd}"))
                .map(|l| l.display().to_string())
                .unwrap_or_default()
        };
        assert!(!crate::io::startup::descriptors_opened());
        assert_eq!(link(0), "/dev/null");
        ensure_native_descriptors();
        assert!(crate::io::startup::descriptors_opened());
        assert_eq!(link(0), "/dev/null");
        assert_eq!(link(3), "anon_inode:[eventpoll]");
    }
}
