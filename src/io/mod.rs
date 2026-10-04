//! OS-level IO mirroring Lean 4.34.0's C runtime (`src/runtime/io.cpp`,
//! `process.cpp`), in safe Rust over std, rustix and nix.
//!
//! # Layout
//!
//! | Module | What |
//! |---|---|
//! | [`error`] | `IO.Error` as neutral data ([`IoError`]), its construction from an `errno` or a libuv code (`decode_io_error`, `decode_uv_error`), the crate's model of C's `errno` |
//! | `sys` | the system calls the FILE model makes, over rustix (crate-internal) |
//! | [`cfile`] | glibc's `FILE` ([`cfile::CFile`]): buffering, read-ahead, positions, the sticky indicators |
//! | [`handle`] | `IO.FS.Handle` ([`Handle`]): open modes, the standard streams, the Lean handle primitives, the open-handle list |
//! | [`exit`] | what a native Lean program's exit does with the streams (libc++'s `ios_base::Init`, then glibc's `_IO_cleanup`), `IO.Process.exit`, `forceExit`, the uncaught-error message |
//! | [`fs`] | the file system: directories, metadata, `realPath`, removal, renaming, links, permissions, the working directory |
//! | [`env`](mod@env) | `IO.getEnv`, `IO.appPath`, the process id, random bytes, the monotonic clock, `IO.sleep` |
//! | [`debug`] | the IO parts of `dbgTrace` and `dbgSleep`, and the runtime's own standard-error lines |
//! | [`startup`] | the descriptors native Lean has open before `main` (libuv's loop), for the translators' ELF constructors |
//! | [`environ`] | the process environment as C's `environ` holds it (every entry in order, changed as glibc's `setenv` and `unsetenv` change it): a child's `envp`, `osEnviron` |
//! | [`process`] | child processes: `IO.Process.spawn` over `posix_spawn`, the `Child` operations, `IO.Process.output` |
//! | [`uvsys`] | `Std.Internal.UV.System`'s queries (libuv 1.48 over std, nix, rustix, `/proc` and `/sys`) |
//! | [`temp`] | `IO.FS.createTempFile` and `createTempDir` |
//! | [`time`] | `timeit`, `Std.Time.Timestamp.now`'s clock, the Windows time-zone externs |
//! | [`streams`] | the calling thread's current standard streams (`IO.getStdout`, `IO.setStdout` & co.) and the route of the runtime's own standard-error lines |
//!
//! The second io batch added [`environ`], [`process`], [`uvsys`], [`temp`],
//! [`time`] and [`streams`].
//!
//! # Rule: no bulk copies in a translator's glue
//!
//! lean2rr keeps `ByteArray` and `String` as its own one-block objects (a
//! header, then the bytes); leanrs keeps `Vec<u8>` and its `Str`. So that
//! neither translator has to copy bulk data between its objects and this
//! crate:
//! - **reads write into the caller's storage**: `read n` fills the `n` bytes
//!   the caller allocated in its own object and returns the count: as
//!   uninitialized memory with no zero pass ([`Handle::read_uninit`], over
//!   rustix's reads into `MaybeUninit` and `write_copy_of_slice`), as a
//!   `Vec` that grows as the bytes come ([`Handle::read_vec`], below), or as
//!   initialized bytes ([`Handle::read`]); an
//!   unbounded result (`getLine`, a path, an
//!   environment value, a directory entry's name) is appended to a
//!   [`ByteSink`] the caller implements on its own object;
//! - **writes take views** (`&[u8]`), never ownership;
//! - **paths and names are views** (`&[u8]`, the bytes of a Lean string).
//!
//! Only the cold error path owns data: [`IoError`] holds its file name and
//! details as `String`s.
//!
//! **How `read_vec` fills a `Vec`** (reviews RIO1-02, RIO1-13; leanrs review
//! F4). `read n` (glibc's `_IO_file_xsgetn`) first copies the `have` bytes
//! the stream's buffer holds; `want = n - have` bytes are then still wanted.
//! If `want < bufsize`, the buffer is refilled and copied from. Otherwise
//! the direct part, `want - want % bufsize` bytes, is read straight from the
//! descriptor, and the remaining `want % bufsize` come through a refill.
//! `bufsize` is the stream's buffer size: the descriptor's `st_blksize`, or
//! 8192 when that is 8192 or more (4096 for ext4 files and pipes here). The
//! `Vec` is not reserved for `n`: it grows with the bytes read, so `read
//! (2^40)` of a 3-byte file touches only those bytes (native's array of `n`
//! bytes is allocated but never written either).
//! - From a **regular file** a handle owns, the direct part is read by std,
//!   `(&file).take(count).read_to_end(v)`, into the spare capacity with no
//!   zero pass, after reserving what the file still holds: the same bytes and
//!   position as glibc's one `read(2)`, in two or three calls. Example:
//!   `IO.FS.readBinFile` of a 1 MiB + 1 byte ext4 file reads `n = 1048577`
//!   on an empty buffer: `have = 0`, `want = 1048577`, a direct part of
//!   1048576 bytes read with `read(8192)` and `read(1040384)`, then the last
//!   byte through a refill; nothing is zeroed.
//! - From **any other descriptor** (a pipe, a standard descriptor, a device),
//!   each `read(2)` of the direct part goes into a zeroed window of at most
//!   the pipe's capacity (`F_GETPIPE_SZ`), so it returns what glibc's single
//!   `read(2)` would, or of at most 64 KiB: a `memset` of at most that much
//!   per call.
//!
//! [`Handle::read_uninit`] fills a translator's own uninitialized allocation
//! (lean2rr's one-block arrays) with no zero pass from any descriptor.

//! # Concurrency
//!
//! Every stream is behind a `std::sync::Mutex`, as glibc locks each `FILE`
//! (native Lean runs `main` on a thread of its own, so glibc's locks are
//! always on). The one call back into a translator under a stream's lock is
//! [`ByteSink::extend_from_slice`] (`getLine` appends while it scans the
//! buffer); the lock is not recursive, so the sink must not call into
//! `lean_runtime::io` nor end the process (see [`ByteSink`]). `Handle::lock`
//! and friends wait in `flock` without holding the stream's lock, as native
//! takes no `FILE` lock for them.
//!
//! # Blocking calls in programs with tasks (sched-io)
//!
//! With the feature `sched`, a call that may block in the kernel (a read of
//! a pipe, a FIFO, a socket or a terminal, a write into one, `Handle.lock`,
//! `Child.wait`, `IO.Process.output`'s waits) lets the scheduler's other
//! contexts run while it waits, as natively only its own thread waits; the
//! system call that follows is the same, so its bytes and errors are the
//! blocking call's. The stream locks are then taken cooperatively: a task
//! blocked in a read keeps its stream, and another context that wants it
//! waits for it without blocking the thread ([`StreamGuard`]). Regular
//! files, a program that has created no task, promise, timer or watch, and
//! a no-suspend scope (`sched::enter_no_suspend`, the translators' free and
//! drop paths) keep the plain calls. The module `coop` (private) has the rules;
//! `docs/sched.md`, "Blocking IO and the event loop", the design and the
//! cases.

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "aarch64", target_arch = "x86_64")
)))]
compile_error!(
    "lean-runtime's io models glibc on Linux (aarch64 and x86-64): its FILE model, errno values and \
     libuv message table are those of that platform"
);

pub mod cfile;
#[cfg(feature = "sched")]
pub(crate) mod coop;
pub mod debug;
pub mod env;
pub mod environ;
pub mod error;
pub mod exit;
pub mod fs;
pub mod handle;
pub mod process;
pub mod startup;
pub mod streams;
mod sys;
pub mod temp;
pub mod time;
pub mod uvsys;

pub use error::IoError;
pub use handle::{FsMode, Handle, StreamGuard};

/// A growable byte buffer a translator implements on its own object, so that
/// an unbounded result is written straight into it (see the module comment).
///
/// Contract: `extend_from_slice` may run while a stream's lock is held
/// (`Handle::get_line`), so it only appends: it must not call into
/// `lean_runtime::io` (the lock is not recursive: a deadlock) and must not end
/// the process (`exit` would wait for that lock). Allocation failure may
/// abort, as Lean's own allocation does.
pub trait ByteSink {
    /// Append `bytes`.
    fn extend_from_slice(&mut self, bytes: &[u8]);
}

impl ByteSink for Vec<u8> {
    #[inline]
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes)
    }
}
