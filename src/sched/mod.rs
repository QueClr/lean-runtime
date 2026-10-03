//! The task scheduler: tasks deferred until needed, run as coroutines that
//! can block and resume (through a vetted coroutine crate), yield points at
//! effect and polling operations, promises, `Std.Sync`, and Lean's behaviour
//! at exit (pending IO tasks run, dropped queued pure tasks never do).
//!
//! Empty until the extraction step.
