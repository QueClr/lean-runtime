//! `lean-runtime`: the behaviour of Lean 4.34.0's runtime, shared by two
//! translators of compiled Lean, lean2rr (Lean to Reussir) and leanrs
//! (Lean to Rust).
//!
//! The crate holds what does not depend on how a translator represents Lean
//! values: pure semantics on views and plain data (`semantics`), OS-level IO
//! (`io`, feature `io`), the task scheduler (`sched`, feature `sched`, on
//! one thread; or feature `threads`, threads mode, on real threads:
//! `sched::mt`, re-exported as `sched`) and networking on the single-thread
//! scheduler's event loop (`net`, feature `net`).
//! Each translator keeps its own value representations, memory protocol and
//! hot paths in its own glue, and calls this crate for the rest.
//!
//! The crate root denies `unsafe` code. It is allowed only in files that
//! say so themselves (`#![allow(unsafe_code)]`), each with an entry in
//! `UNSAFE.md`:
//! - a native quirk that no safe API can reproduce, behind a feature of its
//!   own; so far two: `io::argv_title` (feature `proc-title`, which turns on
//!   `io`): `setProcessTitle` writes the title into the arguments' memory,
//!   as libuv does; and `sched::stack_overflow` (feature `stack-overflow`,
//!   with `sched` or `threads`): Lean's stack-overflow report, a SIGSEGV
//!   handler that knows the scheduler's context stacks;
//! - with the opt-in feature `unsafe-fast`, a faster implementation of a
//!   specific function, with the same observable behaviour as its safe twin.
//!
//! A build with none of `proc-title`, `stack-overflow` and `unsafe-fast`
//! (the default build, `io`, `sched`, `threads`, `net`) compiles no
//! `unsafe` code of the crate: there the root forbids it outright. With any of them, `deny`
//! lets a file allow it for itself, so `scripts/check.sh` checks that every
//! such file has its entry.

#![deny(unsafe_code)]
#![cfg_attr(
    not(any(
        feature = "proc-title",
        feature = "stack-overflow",
        feature = "unsafe-fast"
    )),
    forbid(unsafe_code)
)]

pub mod semantics;

#[cfg(feature = "io")]
pub mod io;

// A build has one scheduler (docs/threads.md, 2.5).
#[cfg(all(feature = "sched", feature = "threads"))]
compile_error!(
    "lean-runtime: the features `threads` (threads mode, `sched::mt`) and `sched` (the \
     single-thread scheduler; `net` turns it on) exclude each other: a build has one scheduler"
);
#[cfg(all(
    feature = "stack-overflow",
    not(any(feature = "sched", feature = "threads"))
))]
compile_error!(
    "lean-runtime: the feature `stack-overflow` reports overflows of a scheduler's stacks: \
     enable `sched` or `threads` with it"
);

// The single-thread scheduler (`src/sched/mod.rs`, docs/sched.md).
#[cfg(feature = "sched")]
pub mod sched;

// Threads mode (`src/sched/threads.rs`, docs/threads.md): `sched::mt`,
// with the single-thread scheduler's names re-exported as `sched::*`.
#[cfg(all(feature = "threads", not(feature = "sched")))]
#[path = "sched/threads.rs"]
pub mod sched;

#[cfg(feature = "net")]
pub mod net;

/// The Lean version whose runtime this crate mirrors. Every expected value in
/// the test suite comes from a native build with this version.
pub const LEAN_VERSION: &str = "4.34.0";
