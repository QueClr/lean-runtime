//! The plain items both schedulers share: the single-thread `sched` (feature
//! `sched`, `src/sched/task.rs`) and threads mode (feature `threads`,
//! `src/sched/mt/`). A build compiles one of them, and this file with it.
//! The functions here call the scheduler of the build through `super`
//! (`sched::wait`, `sched::in_sync_task`), which has the same names in both
//! modes.

/// `IO.TaskState`, its constructors in Lean's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Waiting = 0,
    Running = 1,
    Finished = 2,
}

/// The message of the Lean panic that native `Task.get` prints when it waits
/// for an unfinished task inside a `sync := true` task
/// (`task_manager::wait_for`, `src/runtime/object.cpp`): see `in_sync_task`.
pub const GET_IN_SYNC_TASK: &str = "`Task.get` called from a `(sync := true)` task";

/// The message of Lean's internal panic for `IO.Promise.new` before the task
/// manager runs (`lean_promise_new`); the glue reports it as Lean's
/// `lean_internal_panic` does.
pub const PROMISE_BEFORE_MANAGER: &str = "`IO.Promise.new` called before the task manager is running; this typically happens when called (directly or transitively, e.g. via `IO.CancelToken.new`) from an `initialize` block. Construct lazily on first use instead.";

/// The message of the Lean panic of `IO.Option.getOrBlock!` on `none`
/// (`lean_option_get_or_block`, `io.cpp`), passed to `lean_panic` with
/// `force_stderr`: see `option_get_or_block`.
pub const PROMISE_DROPPED: &str =
    "PANIC: Promise.result!: promise has been dropped without ever being resolved";

/// Priorities: Lean's 0..=8 (`Task.Priority.max`, `LEAN_MAX_PRIO`), and 9
/// for dedicated tasks (a thread of their own natively).
pub(crate) const PRIOS: usize = 10;

/// The queue of a task at Lean priority `prio` (`Task.Priority`, a `Nat`):
/// 0..=8 as they are, and every priority above `Task.Priority.max` (8)
/// dedicated (9), as Lean's documentation says ("Tasks with a priority
/// greater than `Task.Priority.max` are scheduled on dedicated threads",
/// `Init/Core.lean`). The value is the whole `Nat`: a glue passes a
/// priority that fits in a `u64` as it is, and a bigger one (a big `Nat`)
/// saturated to `u64::MAX`, never its low bits; all of them are dedicated.
///
/// No priority makes a task `sync`. Lean has no `sync` spawn: `Task.map`
/// and `Task.bind` take `sync` as an argument of its own, which `depend`
/// receives. Native Lean passes `lean_unbox(prio)` as an `unsigned`, the
/// priority modulo 2^32 (`lean_task_spawn_core`, `lean.h`), so natively
/// 2^32 - 1 is `LEAN_SYNC_PRIO` and runs at once on the spawning thread,
/// and 2^32 to 2^32 + 8 go to the pool: LB-39 of `docs/lean-bugs.md`,
/// which the crate does not copy.
///
/// Examples: `priority(8)` is 8; `priority(9)`, `priority(2^32 - 1)`,
/// `priority(2^32 + 4)` and `priority(u64::MAX)` are 9.
pub(crate) fn priority(prio: u64) -> u8 {
    prio.min(PRIOS as u64 - 1) as u8
}

/// `Task.get` and `IO.wait` (`lean_task_get`, `task_manager::wait_for`) once
/// the glue has found its slot for task `id` empty (the value fast path
/// stays the glue's): inside a `sync` task ([`super::in_sync_task`]),
/// `report(GET_IN_SYNC_TASK)` first, which the glue prints as the Lean panic
/// it is (the program goes on unless `LEAN_ABORT_ON_PANIC`), then
/// [`super::wait`]. For `TaskId::FINISHED` (a task the glue finished
/// itself, such as `Task.pure`) it returns at once and reports nothing, as
/// native's `wait_for` returns when the task has its value. The glue then
/// reads its slot.
///
/// Example: a `sync := true` dependent that calls `Task.get` on a task that
/// has not finished writes the line `` `Task.get` called from a `(sync :=
/// true)` task `` (Lean's `lean_panic` of the message), then waits for it
/// (case `tasks/get_in_sync_task`).
///
/// Source: lean2rr leanrt `src/task.rs` (`await_task`), leanrs_rt
/// `src/task.rs` (`await_value`) and the crate's drivers
/// (`tests/sched-driver*/src/lean.rs`, `Task::get`): the same rule.
pub fn await_task(id: super::TaskId, report: impl FnOnce(&str)) {
    if id == super::TaskId::FINISHED {
        return;
    }
    if super::in_sync_task() {
        report(GET_IN_SYNC_TASK);
    }
    super::wait(id);
}

/// A thread the task manager needs cannot be made (or, in the single-thread
/// scheduler, a context's stack cannot be mapped): natively `lthread`
/// throws `lean::exception("failed to create thread: " << strerror(err))`
/// (`thread.cpp` 135-140), which nothing catches, so libc++ writes its
/// report on standard error and aborts (status 134; no stream is flushed).
/// Native writes the same line for `main`'s thread, a pool worker and a
/// dedicated one. `err` is the error of std's `thread::Builder::spawn`
/// (glibc's `pthread_create`); its `strerror` text ends the line. For a
/// context's stack, pass the mapping's error as `pthread_create` reports a
/// stack it cannot map: `ENOMEM` becomes `EAGAIN`. Example, the usual case
/// (`ulimit -u`, or no memory for the stack): `libc++abi: terminating due to
/// uncaught exception of type lean::exception: failed to create thread:
/// Resource temporarily unavailable`.
///
/// Source: the crate's two copies, `sched/ctx.rs` (the text of `EAGAIN`)
/// and `sched/mt/task.rs` (formatted); the judge's verdict on audit item
/// 5.14 (2026-10-05, a native probe of each thread kind).
pub fn thread_create_failed(err: &std::io::Error) -> ! {
    use std::io::Write;
    let _ = std::io::stderr().write_all(thread_create_failed_line(err).as_bytes());
    std::process::abort()
}

/// The line [`thread_create_failed`] writes: libc++'s report of the
/// uncaught `lean::exception`, with `strerror(err)` (Rust's text of an OS
/// error without its ` (os error N)`) and a newline.
pub(crate) fn thread_create_failed_line(err: &std::io::Error) -> String {
    let text = err.to_string();
    let text = match (err.raw_os_error(), text.rfind(" (os error ")) {
        (Some(_), Some(k)) => &text[..k],
        _ => &text,
    };
    format!(
        "libc++abi: terminating due to uncaught exception of type lean::exception: failed to create thread: {text}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sched::Ref` has one API in both modes (the single-thread type of
    /// wait-1's core 3.2, threads mode's `mt::Ref`): this compiles in each,
    /// and on a full reference no operation waits.
    #[test]
    fn the_ref_api_is_the_same_in_both_modes() {
        use super::super::Ref;
        let r: Ref<String> = Ref::new("a".to_string());
        assert_eq!(r.get(), "a");
        r.set("b".to_string());
        assert_eq!(r.swap("c".to_string()), "b");
        let v = r.take();
        r.put(v + "d");
        r.modify(|v| v + "e");
        let n = r.modify_get(|v| (v.len(), v));
        assert_eq!((n, r.get()), (3, "cde".to_string()));
        let e: Ref<u8> = Ref::empty();
        e.put(1);
        assert_eq!(e.get(), 1);
    }

    /// The whole value counts (LB-39), in both modes: 0..=8 as they are,
    /// everything above dedicated (9), also where native's cut to an
    /// `unsigned` gives `LEAN_SYNC_PRIO` (2^32 - 1) or a pool priority (2^32
    /// to 2^32 + 8), and `u64::MAX`, a big `Nat` as the glue passes it.
    #[test]
    fn priorities() {
        for p in 0..=8 {
            assert_eq!(priority(p), p as u8);
        }
        for p in [
            9,
            1000,
            (1 << 32) - 1,
            1 << 32,
            (1 << 32) + 1,
            (1 << 32) + 8,
            8589934596,
            1 << 63,
            u64::MAX,
        ] {
            assert_eq!(priority(p), 9, "priority {p}");
        }
    }

    /// glibc's `strerror(EAGAIN)`, the text the single-thread scheduler
    /// wrote before (`sched/ctx.rs`).
    #[test]
    fn thread_create_failed_text() {
        assert_eq!(
            thread_create_failed_line(&std::io::Error::from_raw_os_error(11)),
            "libc++abi: terminating due to uncaught exception of type lean::exception: failed to create thread: Resource temporarily unavailable\n"
        );
        assert_eq!(
            thread_create_failed_line(&std::io::Error::from_raw_os_error(12)),
            "libc++abi: terminating due to uncaught exception of type lean::exception: failed to create thread: Cannot allocate memory\n"
        );
    }
}

/// What a wait or a watch waits for on a descriptor: the single-thread
/// loop's (`sched::watch`, `sched::poll_fds`) and, in threads mode,
/// `sched::uv`'s io watchers for `net` (docs/threads.md, 0.7).
#[cfg(any(feature = "sched", feature = "net"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Interest {
    /// Readable (`POLLIN`; end of file and errors count).
    pub read: bool,
    /// Writable (`POLLOUT`; errors and a hang-up count).
    pub write: bool,
}

#[cfg(any(feature = "sched", feature = "net"))]
// in threads mode the constants are crate-private, and `net` names them only
// in its tests
#[cfg_attr(not(feature = "sched"), allow(dead_code))]
impl Interest {
    pub const READ: Interest = Interest {
        read: true,
        write: false,
    };
    pub const WRITE: Interest = Interest {
        read: false,
        write: true,
    };
    pub const BOTH: Interest = Interest {
        read: true,
        write: true,
    };

    pub(crate) fn epoll_flags(self) -> rustix::event::epoll::EventFlags {
        use rustix::event::epoll::EventFlags as E;
        let mut f = E::empty();
        if self.read {
            f |= E::IN;
        }
        if self.write {
            f |= E::OUT;
        }
        f
    }
}

/// What the loop saw on a descriptor.
#[cfg(any(feature = "sched", feature = "net"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ready {
    /// A read would not block (data, end of file, or an error).
    pub read: bool,
    /// A write would not block (room, or an error).
    pub write: bool,
    /// `POLLHUP`: the other end is closed.
    pub hangup: bool,
    /// `POLLERR`.
    pub error: bool,
}

#[cfg(any(feature = "sched", feature = "net"))]
impl Ready {
    pub(crate) fn from_epoll(f: rustix::event::epoll::EventFlags) -> Ready {
        use rustix::event::epoll::EventFlags as E;
        let error = f.contains(E::ERR);
        let hangup = f.contains(E::HUP);
        Ready {
            read: f.intersects(E::IN | E::PRI | E::RDHUP) || error || hangup,
            write: f.contains(E::OUT) || error || hangup,
            hangup,
            error,
        }
    }

    /// Whether this answers `i`.
    pub fn meets(self, i: Interest) -> bool {
        (i.read && self.read) || (i.write && self.write)
    }
}
