//! `lean-runtime`: the behaviour of Lean 4.34.0's runtime, shared by two
//! translators of compiled Lean, lean2rr (Lean to Reussir) and leanrs
//! (Lean to Rust).
//!
//! The crate holds what does not depend on how a translator represents Lean
//! values: pure semantics on views and plain data (`semantics`), OS-level IO
//! (`io`, feature `io`), the task scheduler (`sched`, feature `sched`) and
//! networking on its event loop (`net`, feature `net`).
//! Each translator keeps its own value representations, memory protocol and
//! hot paths in its own glue, and calls this crate for the rest.
//!
//! The crate root denies `unsafe` code. It is allowed only in files that
//! say so themselves (`#![allow(unsafe_code)]`), each with an entry in
//! `UNSAFE.md`:
//! - a native quirk that no safe API can reproduce, behind a feature of its
//!   own; so far one, `io::argv_title` (feature `proc-title`, which turns on
//!   `io`): `setProcessTitle` writes the title into the arguments' memory,
//!   as libuv does;
//! - with the opt-in feature `unsafe-fast`, a faster implementation of a
//!   specific function, with the same observable behaviour as its safe twin.
//!
//! A build with neither `proc-title` nor `unsafe-fast` (the default build,
//! `io`, `sched`, `net`) compiles no `unsafe` code of the crate: there the
//! root forbids it outright. With either, `deny` lets a file allow it for
//! itself, so `scripts/check.sh` checks that every such file has its entry.

#![deny(unsafe_code)]
#![cfg_attr(
    not(any(feature = "proc-title", feature = "unsafe-fast")),
    forbid(unsafe_code)
)]

pub mod semantics;

#[cfg(feature = "io")]
pub mod io;

#[cfg(feature = "sched")]
pub mod sched;

#[cfg(feature = "net")]
pub mod net;

/// The Lean version whose runtime this crate mirrors. Every expected value in
/// the test suite comes from a native build with this version.
pub const LEAN_VERSION: &str = "4.34.0";
