//! `lean-runtime`: the behaviour of Lean 4.34.0's runtime, shared by two
//! translators of compiled Lean, lean2rr (Lean to Reussir) and leanrs
//! (Lean to Rust).
//!
//! The crate holds what does not depend on how a translator represents Lean
//! values: pure semantics on views and plain data (`semantics`), OS-level IO
//! (`io`, feature `io`) and the task scheduler (`sched`, feature `sched`).
//! Each translator keeps its own value representations, memory protocol and
//! hot paths in its own glue, and calls this crate for the rest.
//!
//! The default build contains no `unsafe` code. The opt-in feature
//! `unsafe-fast` may enable faster implementations of specific functions, each
//! with the same observable behaviour as its safe twin (see `UNSAFE.md`).

#![cfg_attr(not(feature = "unsafe-fast"), forbid(unsafe_code))]
#![cfg_attr(feature = "unsafe-fast", deny(unsafe_code))]

pub mod semantics;

#[cfg(feature = "io")]
pub mod io;

#[cfg(feature = "sched")]
pub mod sched;

/// The Lean version whose runtime this crate mirrors. Every expected value in
/// the test suite comes from a native build with this version.
pub const LEAN_VERSION: &str = "4.34.0";
