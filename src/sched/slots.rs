//! The thread-local state each task sees natively (review AR-24), on the
//! single-thread scheduler (features `sched` and `io`): its thread's current
//! standard streams (`IO.setStdout` & co.) and its `errno`, which the io
//! layer keeps in thread-locals (`io::streams`, `io::error`), together a
//! `ThreadSlots`.
//!
//! Natively they belong to the OS thread: `main` has its own; a pool worker
//! keeps them from one task to the next, so a task that sets a stream, or
//! leaves `errno` set, and does not restore it, leaves it to the next task
//! of that worker (Lean's docs: `IO.setStdout` replaces "the stdout of the
//! current thread"; `io.cpp` 115-117, `MK_THREAD_LOCAL_GET`); a new worker,
//! and a dedicated task's thread of its own, start with the process's
//! streams and `errno` 0; libuv's loop thread keeps its own. Here every
//! context runs on one thread, so the scheduler swaps the sets:
//! - **per context**: the hub swaps a context's set in before it resumes the
//!   context, and out after the context suspends or ends (`ContextSlots`),
//!   so a context's streams are never another's, whatever runs meanwhile;
//!   `main`'s are the thread's own. A new context starts with a fresh set,
//!   except the event loop's, which takes the set the last loop context
//!   left (native's one loop thread);
//! - **per task** (`TaskSlots`): a task that natively runs on a thread of its
//!   own (`own`: a pool task, a dedicated task; not a `sync` task, which
//!   shares the thread it runs on) runs with an emulated thread's set: a
//!   pool task with the set of the lowest free worker id (free: no task of
//!   it runs, a task waiting in `Task.get` included), which keeps the set
//!   the task leaves at its end; a dedicated task with a fresh set, dropped
//!   at its end. At `LEAN_NUM_THREADS=1` with one task at a time, that is
//!   one set for every pool task, as natively.
//!
//! Which worker natively takes a task is the schedule's choice (an idle
//! worker woken by `notify_one`); the lowest free id is one of its outcomes,
//! and the one the recorded cases show (`tasks/worker_keeps_streams`,
//! `worker_keeps_errno`).
//!
//! No translator code runs under a borrow of the store: a set that leaves
//! it is dropped after the borrow (a stream's drop is translator code). At
//! thread exit the store's sets are forgotten, not dropped, as the
//! scheduler's other translator values are (natively nothing is destructed
//! at exit).

use crate::io::streams::ThreadSlots;
use std::cell::RefCell;

#[derive(Default)]
struct Store {
    /// A context's set while it does not run, by context index.
    contexts: Vec<Option<ThreadSlots>>,
    /// The event loop context's set between two loop contexts.
    event_loop: Option<ThreadSlots>,
    /// The emulated pool workers' sets, by worker id: `None` while a task
    /// of that worker runs.
    workers: Vec<Option<ThreadSlots>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        for s in self.contexts.drain(..).chain(self.workers.drain(..)) {
            std::mem::forget(s);
        }
        std::mem::forget(self.event_loop.take());
    }
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

/// The hub's swap for context `n` (its index), from just before it resumes
/// to just after it suspends or ends. While the context runs, `held` is the
/// set of `main`'s context (the hub's); after, the context's own.
pub(crate) struct ContextSlots {
    n: usize,
    event_loop: bool,
    held: Option<ThreadSlots>,
}

impl ContextSlots {
    /// Context `n` (the event loop's when `event_loop`) is about to run: its
    /// set is swapped in (a fresh one for a new context; the last loop
    /// context's for a new loop context).
    pub(crate) fn enter(n: usize, event_loop: bool) -> ContextSlots {
        let mut held = STORE
            .try_with(|s| {
                let mut s = s.borrow_mut();
                let saved = s.contexts.get_mut(n).and_then(Option::take);
                match saved {
                    Some(x) => x,
                    None if event_loop => s.event_loop.take().unwrap_or_default(),
                    None => ThreadSlots::default(),
                }
            })
            .unwrap_or_default();
        held.swap();
        ContextSlots {
            n,
            event_loop,
            held: Some(held),
        }
    }

    /// The context has suspended (`ended` false) or ended: `main`'s set
    /// comes back, and the context's is kept for its next resume, or, once
    /// it has ended, kept for the next loop context or dropped.
    pub(crate) fn leave(mut self, ended: bool) {
        if let Some(mut set) = self.held.take() {
            set.swap();
            keep(self.n, self.event_loop, ended, set);
        }
    }
}

impl Drop for ContextSlots {
    /// A Rust panic unwinds the hub from the context (S6): `main`'s set
    /// comes back; the dead context's goes.
    fn drop(&mut self) {
        if let Some(mut set) = self.held.take() {
            set.swap();
            keep(self.n, self.event_loop, true, set);
        }
    }
}

/// Keeps context `n`'s set, or drops it (after the store's borrow).
fn keep(n: usize, event_loop: bool, ended: bool, set: ThreadSlots) {
    let mut set = Some(set);
    let _ = STORE.try_with(|s| {
        let mut s = s.borrow_mut();
        if !ended {
            if s.contexts.len() <= n {
                s.contexts.resize_with(n + 1, || None);
            }
            s.contexts[n] = set.take();
        } else if event_loop {
            s.event_loop = set.take();
        }
    });
    drop(set);
}

/// A task's emulated thread, from its start to its end (`run_task`): the set
/// of the thread below it (the context's, or the task's that waits for it
/// on its own stack) is held here meanwhile.
pub(crate) struct TaskSlots {
    /// The worker id of a pool task; `None` for a dedicated task.
    worker: Option<usize>,
    held: Option<ThreadSlots>,
}

impl TaskSlots {
    /// Task begins: with `own` (natively on a thread of its own) its
    /// emulated thread's set is swapped in, a pool task's from the lowest
    /// free worker id (`dedicated` false), a dedicated task's fresh; a `sync`
    /// task (`own` false) keeps the current set.
    pub(crate) fn begin(own: bool, dedicated: bool) -> TaskSlots {
        if !own {
            return TaskSlots {
                worker: None,
                held: None,
            };
        }
        let (worker, mut held) = if dedicated {
            (None, ThreadSlots::default())
        } else {
            let taken = STORE.try_with(|s| {
                let mut s = s.borrow_mut();
                match s.workers.iter().position(Option::is_some) {
                    Some(w) => (w, s.workers[w].take().unwrap_or_default()),
                    None => {
                        s.workers.push(None);
                        (s.workers.len() - 1, ThreadSlots::default())
                    }
                }
            });
            match taken {
                Ok((w, x)) => (Some(w), x),
                Err(_) => (None, ThreadSlots::default()),
            }
        };
        held.swap();
        TaskSlots {
            worker,
            held: Some(held),
        }
    }
}

impl Drop for TaskSlots {
    /// The task has ended (its `sync` dependents run), or waits for the task
    /// its bind function returned, or a panic unwinds it: the thread below
    /// gets its set back; the worker keeps the task's, a dedicated task's
    /// goes.
    fn drop(&mut self) {
        let Some(mut set) = self.held.take() else {
            return;
        };
        set.swap();
        let mut set = Some(set);
        if let Some(w) = self.worker {
            let _ = STORE.try_with(|s| {
                if let Some(slot) = s.borrow_mut().workers.get_mut(w) {
                    *slot = set.take();
                }
            });
        }
        drop(set);
    }
}
