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
//! | [`env`] | `IO.getEnv`, `IO.appPath`, the process id, random bytes, the monotonic clock, `IO.sleep` |
//! | [`debug`] | the IO parts of `dbgTrace` and `dbgSleep`, and the runtime's own standard-error lines |
//!
//! The second io batch adds processes, `Std.Time`, temporary files, the
//! `Std.Internal.UV.System` queries and the redirection of the standard
//! streams (`IO.setStdout` & co.) as modules of their own.
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
//!   `Vec`'s spare capacity ([`Handle::read_vec`]), or as initialized bytes
//!   ([`Handle::read`]); an unbounded result (`getLine`, a path, an
//!   environment value, a directory entry's name) is appended to a
//!   [`ByteSink`] the caller implements on its own object;
//! - **writes take views** (`&[u8]`), never ownership;
//! - **paths and names are views** (`&[u8]`, the bytes of a Lean string).
//!
//! Only the cold error path owns data: [`IoError`] holds its file name and
//! details as `String`s.
//!
//! # Concurrency
//!
//! Every stream is behind a `std::sync::Mutex`, as glibc locks each `FILE`
//! (native Lean runs `main` on a thread of its own, so glibc's locks are
//! always on). Nothing here calls back into a translator while holding a
//! lock.

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "aarch64", target_arch = "x86_64")
)))]
compile_error!(
    "lean-runtime's io models glibc on Linux (aarch64 and x86-64): its FILE model, errno values and \
     libuv message table are those of that platform"
);

pub mod cfile;
pub mod debug;
pub mod env;
pub mod error;
pub mod exit;
pub mod fs;
pub mod handle;
mod sys;

pub use error::IoError;
pub use handle::{FsMode, Handle};

/// A growable byte buffer a translator implements on its own object, so that
/// an unbounded result is written straight into it (see the module comment).
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
