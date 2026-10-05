//! The module `sched` of a build with the feature `threads` (threads mode,
//! docs/threads.md): Lean's task manager on real threads, `mt`. A build has
//! one scheduler, so this file stands in for `src/sched/mod.rs` (the
//! single-thread scheduler, feature `sched`), and the two features exclude
//! each other (a compile error in `src/lib.rs`).
//!
//! `mt`'s items are re-exported here under the single-thread scheduler's
//! names (`sched::spawn`, `sched::wait`, `sched::Job`, `sched::sync`, ...),
//! so that a translator's glue compiles against either mode with a `cfg` on
//! its `Glue` implementation and its `start` call only (leanrs's
//! constraints for T1, point 1). What only the single-thread scheduler has
//! (`Glue::suspend`, contexts, `block_sync` and `wake`, the event loop's
//! `watch` and `timer_start`) is not here: a blocked task blocks its own
//! thread. `sched::uv` is `mt::uv`: `Std.Internal.UV`'s loop on a thread of
//! its own, as natively, with the single-thread module's names (T2).
//!
//! Shared with the single-thread scheduler: `common` (the task states, the
//! messages, the priorities), `env` (`LEAN_NUM_THREADS`,
//! `LEAN_STACK_SIZE_KB`) and, with the feature `stack-overflow`,
//! `stack_overflow` (Lean's report, a native quirk with `unsafe`; UNSAFE.md).

mod common;
// The deferred promise resolutions of a translator's drains (wait-1, core
// 3.3), as in the single-thread scheduler.
mod drain;
mod env;
pub mod mt;
// The process-wide part of `uv`'s signal delivery (`mt::uv`, re-exported as
// `sched::uv`), shared with the single-thread scheduler.
mod uv_signals;
// `publish` is the single-thread hub's: threads mode has no context to
// publish, so it is unused here.
#[cfg(feature = "stack-overflow")]
#[allow(dead_code)]
mod stack_overflow;

/// What `stack_overflow` reads of the context running on a thread: in
/// threads mode none (every task runs on a thread's own stack), so a
/// registered thread's record holds only its own guard.
#[cfg(feature = "stack-overflow")]
mod ctx {
    pub(crate) use super::mt::{running_stack, StackBounds};
}

pub use common::{await_task, thread_create_failed};
pub use drain::{
    defer, deferred_pending, run_deferred, Deferred, DrainScope, RESOLVE_IN_NO_SUSPEND,
};
pub use env::{hardware_concurrency, lean_num_threads, thread_stack_size};
pub use mt::*;
#[cfg(feature = "stack-overflow")]
pub use stack_overflow::install_stack_overflow_handler;
