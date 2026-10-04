//! The calling thread's current standard streams: `IO.getStdin`,
//! `getStdout`, `getStderr` and `IO.setStdin`, `setStdout`, `setStderr`
//! (`lean_get_stdin` & co. and `lean_get_set_stdin` & co. in Lean 4.34.0's
//! `io.cpp`), and the route of the runtime's own standard-error lines.
//!
//! Natively every thread has its own current streams (`MK_THREAD_LOCAL_GET`
//! over `g_stream_stdin` & co.): a thread starts with the process's streams
//! (`IO.FS.Stream.ofHandle` over the standard handles), and `setStdout`
//! replaces the calling thread's and returns the previous one.
//! `IO.FS.withIsolatedStreams` is Lean code over these.
//!
//! `IO.FS.Stream` is a structure of closures in each translator's own
//! representation, so the slots here are type-erased (`Rc<dyn Any>`) and the
//! functions generic over the stream type `S`. The translator passes, as
//! `default`, the stream a thread starts with. `getStdout` returns a clone of
//! `S`, so `S` should be one reference-counted pointer (an `Rc` or `Arc` to
//! the translator's stream object, as Lean's `object_ref`), whose clone is a
//! count increment; a structure of several closures would clone each.
//!
//! A task that runs as a coroutine on a thread that other tasks share has its
//! own slots: [`swap_context`] exchanges the thread's slots with the task's,
//! and a new task starts from [`StreamContext::default`], the process's
//! streams. Natively a task runs on a pool worker whose slots persist from
//! one task to the next, so a task that sets a stream and does not restore it
//! leaves it to whichever task the worker runs next; which one that is
//! depends on the schedule. Starting each task from the process's streams is
//! one of native's outcomes (a fresh worker's) and the one a program can rely
//! on: a schedule-dependent difference, documented, not a semantic one.
//!
//! The runtime's own standard-error lines (panic messages, `dbgTrace`,
//! `timeit`) go to the current standard error stream, as Lean's
//! `io_eprintln` writes them with `IO.eprintln`: [`set_stderr`] also takes the
//! new stream's `putStr`, which [`put_current_stderr`] calls with the line;
//! while no stream has been set, the line goes to glibc's `stderr`
//! ([`super::debug::runtime_eprintln`]). Standard error is set only through
//! [`set_stderr`], so the stream and the route never part.
//!
//! No translator code runs while a slot is borrowed: clones of `S`, the
//! default and the `putStr` are called outside the borrow, so they may use
//! the streams themselves. While the thread's storage is being destroyed (a
//! thread's own exit), the slots are gone: reads give `default()`, sets are
//! dropped, and the runtime's lines go to glibc's `stderr`.
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/streams.rs` (type-erased
//! thread-local slots, the `putStr` route; probe `validate/io/stream_redirect`)
//! and lean2rr's stream cells (`lean2rr/LeanToReussir/Lower/Externs.lean`,
//! `stdStreamFns`, `stdContextFns`: a cell per stream, built on first use,
//! set aside while a task runs).

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

/// One of the three standard streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdStream {
    Stdin = 0,
    Stdout = 1,
    Stderr = 2,
}

/// The `putStr` of the current standard error stream, called with a line.
pub type StderrPut = Rc<dyn Fn(&[u8])>;

/// A thread's (or a task's) current standard streams.
#[derive(Default)]
pub struct StreamContext {
    streams: [Option<Rc<dyn Any>>; 3],
    stderr_put: Option<StderrPut>,
}

thread_local! {
    static CURRENT: RefCell<StreamContext> = RefCell::new(StreamContext::default());
}

fn wrong_type() -> ! {
    panic!("lean-runtime: a standard stream of another type than the one set")
}

/// The stream a slot holds: taken out when the slot held the only reference,
/// else cloned.
fn unerase<S: Clone + 'static>(held: Rc<dyn Any>) -> S {
    match held.downcast::<S>() {
        Ok(rc) => Rc::try_unwrap(rc).unwrap_or_else(|rc| (*rc).clone()),
        Err(_) => wrong_type(),
    }
}

/// `IO.getStdin`, `getStdout`, `getStderr`: a clone of the calling thread's
/// current stream, `default()` (then kept) while none has been set.
pub fn current<S: Clone + 'static>(which: StdStream, default: impl FnOnce() -> S) -> S {
    // the slot's `Rc` is cloned under the borrow, `S` outside it
    let held = CURRENT
        .try_with(|c| c.borrow().streams[which as usize].clone())
        .ok()
        .flatten();
    if let Some(held) = held {
        return match held.downcast::<S>() {
            Ok(rc) => (*rc).clone(),
            Err(_) => wrong_type(),
        };
    }
    let s = default();
    let kept: Rc<dyn Any> = Rc::new(s.clone());
    let displaced = CURRENT.try_with(|c| c.borrow_mut().streams[which as usize].replace(kept));
    drop(displaced);
    s
}

/// Makes `s` the stream `which` and returns the previous one.
fn set<S: Clone + 'static>(which: StdStream, s: S, default: impl FnOnce() -> S) -> S {
    let fresh: Rc<dyn Any> = Rc::new(s);
    let prev = CURRENT
        .try_with(|c| c.borrow_mut().streams[which as usize].replace(fresh))
        .ok()
        .flatten();
    match prev {
        Some(held) => unerase(held),
        None => default(),
    }
}

/// `IO.setStdin`: `s` becomes the calling thread's current standard input;
/// the previous one is returned, `default()` while none had been set.
pub fn set_stdin<S: Clone + 'static>(s: S, default: impl FnOnce() -> S) -> S {
    set(StdStream::Stdin, s, default)
}

/// `IO.setStdout`, as [`set_stdin`].
pub fn set_stdout<S: Clone + 'static>(s: S, default: impl FnOnce() -> S) -> S {
    set(StdStream::Stdout, s, default)
}

/// `IO.setStderr`: as [`set_stdin`], and `put`, the new stream's `putStr`,
/// becomes the route of the runtime's standard-error lines on this thread.
pub fn set_stderr<S: Clone + 'static>(s: S, put: StderrPut, default: impl FnOnce() -> S) -> S {
    let old_put = CURRENT.try_with(|c| c.borrow_mut().stderr_put.replace(put));
    drop(old_put);
    set(StdStream::Stderr, s, default)
}

/// Lean's `io_eprintln` for a line (with its `\n`) when the calling thread's
/// standard error stream has been set: the stream's `putStr` writes it (its
/// result is dropped, as `io_eprintln` drops it) and `true` is returned;
/// `false` while none has been set (or while the thread's storage is being
/// destroyed), the line then being the caller's to write to glibc's
/// `stderr`.
pub fn put_current_stderr(line: &[u8]) -> bool {
    let put = CURRENT
        .try_with(|c| c.borrow().stderr_put.clone())
        .ok()
        .flatten();
    match put {
        Some(put) => {
            put(line);
            true
        }
        None => false,
    }
}

/// Exchanges the calling thread's current streams with `ctx`: a scheduler
/// running tasks as coroutines on one thread calls it when it switches to a
/// task and back, so each task has a thread's streams of its own (a new task
/// starts with `StreamContext::default()`, see the module comment).
pub fn swap_context(ctx: &mut StreamContext) {
    let _ = CURRENT.try_with(|c| std::mem::swap(&mut *c.borrow_mut(), ctx));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The default until a stream is set, the previous one returned
    /// (leanrs's `io_current_streams`).
    #[test]
    fn current_and_set() {
        assert_eq!(current(StdStream::Stdout, || 1u8), 1);
        assert_eq!(set_stdout(2u8, || 1), 1);
        assert_eq!(current(StdStream::Stdout, || 1u8), 2);
        assert_eq!(set_stdout(3u8, || 1), 2);
        assert_eq!(current(StdStream::Stderr, || 7u8), 7);
        assert_eq!(set_stdin(Rc::new(5u8), || Rc::new(0)), Rc::new(0));
        assert_eq!(*current(StdStream::Stdin, || Rc::new(0u8)), 5);
    }

    /// The runtime's lines go through the stream set last; a task's context
    /// starts with the process's streams (leanrs's
    /// `io_stderr_stream_receives_runtime_text`; native case `task_streams`).
    #[test]
    fn stderr_route_and_contexts() {
        let seen: Rc<RefCell<Vec<u8>>> = Rc::default();
        let sink = seen.clone();
        assert!(!put_current_stderr(b"to glibc's stderr\n"));
        assert_eq!(
            set_stderr(
                1u32,
                Rc::new(move |l: &[u8]| sink.borrow_mut().extend_from_slice(l)),
                || 0
            ),
            0
        );
        assert!(put_current_stderr(b"PANIC\n"));
        let mut task = StreamContext::default();
        swap_context(&mut task);
        assert!(!put_current_stderr(b"task line\n"));
        assert_eq!(current(StdStream::Stderr, || 9u32), 9);
        swap_context(&mut task);
        assert_eq!(current(StdStream::Stderr, || 0u32), 1);
        assert!(put_current_stderr(b"again\n"));
        assert_eq!(*seen.borrow(), b"PANIC\nagain\n");
        // a stream's putStr may itself set streams
        assert_eq!(
            set_stderr(
                2u32,
                Rc::new(|_: &[u8]| {
                    let _ = set_stdout(4u8, || 0);
                }),
                || 0
            ),
            1
        );
        assert!(put_current_stderr(b"x\n"));
        assert_eq!(current(StdStream::Stdout, || 0u8), 4);
    }

    /// A stream whose clone uses the streams (translator code may): cloned
    /// outside the slots' borrow, so no `RefCell` panic.
    #[derive(Debug, PartialEq)]
    struct Reentrant(u8);

    impl Clone for Reentrant {
        fn clone(&self) -> Self {
            let _ = current(StdStream::Stdin, || 0u16);
            Reentrant(self.0)
        }
    }

    #[test]
    fn clone_outside_the_borrow() {
        assert_eq!(current(StdStream::Stdout, || Reentrant(1)), Reentrant(1));
        assert_eq!(current(StdStream::Stdout, || Reentrant(9)), Reentrant(1));
        let shared = current(StdStream::Stdout, || Reentrant(9));
        assert_eq!(set_stdout(Reentrant(2), || Reentrant(0)), shared);
    }

    /// A stream dropped while the thread's storage is destroyed, whose drop
    /// writes a runtime line: the slots are gone, so the line goes to
    /// glibc's `stderr` (`false`), with no panic.
    #[derive(Clone)]
    struct Sentinel(Option<Arc<AtomicUsize>>);

    impl Drop for Sentinel {
        fn drop(&mut self) {
            if let Some(f) = &self.0 {
                f.store(
                    1 + usize::from(put_current_stderr(b"late\n")),
                    Ordering::SeqCst,
                );
            }
        }
    }

    #[test]
    fn thread_storage_destroyed() {
        let flag = Arc::new(AtomicUsize::new(0));
        let f = flag.clone();
        std::thread::spawn(move || {
            let _ = set_stderr(Sentinel(Some(f)), Rc::new(|_: &[u8]| {}), || Sentinel(None));
        })
        .join()
        .unwrap();
        assert_eq!(flag.load(Ordering::SeqCst), 1);
    }
}
