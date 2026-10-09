//! Unit tests of threads mode, small sizes. Each test has a task manager of
//! its own (`bind_local`), bound to the test's thread and to the threads it
//! makes, so tests run in parallel; each ends with `finish`, which joins
//! every thread it made (Miri runs these tests and checks that no thread is
//! left). Gates (a lock and a condition variable of std) hold a task where
//! a test needs it, so the checks do not depend on timing; the few sleeps
//! only make the other order likely, never required.

use super::sync::{Condvar, Mutex, RecursiveMutex, SharedMutex};
use super::task::{
    bind_local, blocked_waits, configure, live_workers, table_len, wake_waiters, Shared,
};
use super::*;
use crate::sched::await_task;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};

struct TestGlue;

impl Glue for TestGlue {}

/// With the feature `stack-overflow`, every thread a manager makes
/// registers with Lean's report (`thread_entry`); a thread that ends frees
/// its record, which a new thread may then reuse (glibc reuses a thread's
/// `errno` address). So these tests run one at a time with the report's
/// own, which check records by key.
/// Without that feature they run one at a time all the same: each makes
/// threads, and the host is shared.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    #[cfg(feature = "stack-overflow")]
    let m = &crate::sched::stack_overflow::tests::SERIAL;
    #[cfg(not(feature = "stack-overflow"))]
    let m = {
        static SERIAL: StdMutex<()> = StdMutex::new(());
        &SERIAL
    };
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A task manager of this test's own, with `workers` standard workers and
/// small thread stacks.
fn start_test(workers: u32) -> Arc<Shared> {
    let sh = bind_local();
    configure(&sh, Arc::new(TestGlue), workers, 256 << 10);
    sh
}

type Log = Arc<StdMutex<Vec<String>>>;

fn log() -> Log {
    Arc::new(StdMutex::new(Vec::new()))
}

fn push(l: &Log, s: &str) {
    l.lock().unwrap().push(s.to_string());
}

fn entries(l: &Log) -> Vec<String> {
    l.lock().unwrap().clone()
}

fn job(l: &Log, s: &str) -> Job {
    let l = l.clone();
    let s = s.to_string();
    Box::new(move || {
        l.lock().unwrap().push(s);
        Outcome::Done
    })
}

/// A task's value as a translator keeps it: a slot the job fills.
type Slot<T> = Arc<OnceLock<T>>;

fn filling<T: Send + Sync + 'static>(
    slot: &Slot<T>,
    f: impl FnOnce() -> T + Send + 'static,
) -> Job {
    let slot = slot.clone();
    Box::new(move || {
        let _ = slot.set(f());
        Outcome::Done
    })
}

/// Opened once; whoever waits returns from then on.
#[derive(Clone, Default)]
struct Gate(Arc<(StdMutex<bool>, std::sync::Condvar)>);

impl Gate {
    fn open(&self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }
    fn wait(&self) {
        let mut g = self.0 .0.lock().unwrap();
        while !*g {
            g = self.0 .1.wait(g).unwrap();
        }
    }
    /// `wait`, for at most `d`: whether it opened.
    fn wait_at_most(&self, d: std::time::Duration) -> bool {
        let g = self.0 .0.lock().unwrap();
        let (g, _) = self.0 .1.wait_timeout_while(g, d, |open| !*open).unwrap();
        *g
    }
}

/// Sets its flag when dropped.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A pool task that holds its worker until `release` opens.
fn blocker(l: &Log, started: &Gate, release: &Gate) -> TaskId {
    let (l, started, release) = (l.clone(), started.clone(), release.clone());
    spawn(
        Box::new(move || {
            started.open();
            release.wait();
            push(&l, "blocker");
            Outcome::Done
        }),
        0,
        true,
    )
}

/// Wait until `cond` holds (looking every millisecond).
fn until(cond: impl Fn() -> bool) {
    while !cond() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn before_the_task_manager_tasks_run_at_once() {
    let l = log();
    let id = spawn(job(&l, "init task"), 0, true);
    assert_eq!(id, TaskId::FINISHED);
    assert!(is_finished(id));
    assert_eq!(entries(&l), ["init task"]);
    assert!(!manager_running());
    assert_eq!(promise_new(), Err(PROMISE_BEFORE_MANAGER));
    assert!(dependent_runs_now(TaskId::FINISHED, false));
    assert_eq!(state(id), TaskState::Finished);
}

#[test]
fn no_task_manager_with_zero_workers() {
    // LEAN_NUM_THREADS=0: `lean_init_task_manager_using(0)` creates none.
    let _s = serial();
    let sh = start_test(0);
    assert!(!manager_running());
    let l = log();
    assert_eq!(spawn(job(&l, "a"), 0, false), TaskId::FINISHED);
    assert_eq!(entries(&l), ["a"]);
    assert!(promise_new().is_err());
    finish();
    assert_eq!(live_workers(&sh), 0);
}

#[test]
fn tasks_run_on_workers_and_wait_returns_their_values() {
    let _s = serial();
    let sh = start_test(2);
    assert!(manager_running());
    let slots: Vec<Slot<u32>> = (0..4).map(|_| Slot::default()).collect();
    let ids: Vec<TaskId> = slots
        .iter()
        .enumerate()
        .map(|(k, s)| spawn(filling(s, move || k as u32 * 10), 0, k % 2 == 0))
        .collect();
    for (k, id) in ids.iter().enumerate() {
        wait(*id);
        assert_eq!(slots[k].get(), Some(&(k as u32 * 10)));
        assert!(is_finished(*id));
        assert_eq!(state(*id), TaskState::Finished);
    }
    // the pool's limit: two workers at most
    assert!(live_workers(&sh) <= 2);
    finish();
    assert_eq!(live_workers(&sh), 0);
    assert_eq!(table_len(&sh), 0);
    assert!(!manager_running());
    // after `finish`, tasks run at once again
    let l = log();
    assert_eq!(spawn(job(&l, "late"), 0, true), TaskId::FINISHED);
    assert_eq!(entries(&l), ["late"]);
}

#[test]
fn queues_go_by_priority_then_first_come() {
    let _s = serial();
    start_test(1);
    let l = log();
    let (started, go) = (Gate::default(), Gate::default());
    let b = blocker(&l, &started, &go);
    started.wait();
    // the only worker is busy: these wait in their queues
    let c0 = spawn(job(&l, "c0"), 0, true);
    let b0 = spawn(job(&l, "b0"), 0, true);
    let a8 = spawn(job(&l, "a8"), 8, true);
    let m4 = spawn(job(&l, "m4"), 4, true);
    assert_eq!(state(c0), TaskState::Waiting);
    assert_eq!(state(b), TaskState::Running);
    // a dedicated task has a thread of its own
    let d = spawn(job(&l, "dedicated"), 9, true);
    wait(d);
    assert_eq!(entries(&l), ["dedicated"]);
    // and so has every priority above 8, whatever its low 32 bits (LB-39):
    // natively 2^32 - 1 is `LEAN_SYNC_PRIO` and 2^32 + 4 waits in queue 4
    for (prio, name) in [
        (u64::from(u32::MAX), "2^32-1"),
        ((1 << 32) + 4, "2^32+4"),
        (u64::MAX, "big"),
    ] {
        let d = spawn(job(&l, name), prio, true);
        wait(d);
    }
    assert_eq!(entries(&l), ["dedicated", "2^32-1", "2^32+4", "big"]);
    go.open();
    for id in [c0, b0, a8, m4] {
        wait(id);
    }
    assert_eq!(
        entries(&l),
        [
            "dedicated",
            "2^32-1",
            "2^32+4",
            "big",
            "blocker",
            "a8",
            "m4",
            "c0",
            "b0"
        ]
    );
    finish();
}

/// No priority runs a spawn at once on the calling thread (LB-39): 2^32 - 1
/// and a big priority are dedicated tasks, on a thread of their own (no
/// pool worker's) and no `sync` tasks. `sync` alone makes a dependent run at once on the thread
/// that finishes its source (here the resolving one), whatever its
/// priority.
#[test]
fn only_sync_runs_a_task_on_the_calling_thread() {
    let _s = serial();
    start_test(2);
    let me = thread_number();
    for prio in [u64::from(u32::MAX), u64::MAX] {
        let seen: Slot<(u64, bool, Option<u32>)> = Slot::default();
        let id = spawn(
            filling(&seen, || {
                (thread_number(), in_sync_task(), running_worker())
            }),
            prio,
            false,
        );
        wait(id);
        let &(th, sync, worker) = seen.get().expect("it ran");
        assert_ne!(th, me, "priority {prio} ran on the calling thread");
        assert!(!sync, "priority {prio} ran as a sync task");
        assert_eq!(worker, None, "priority {prio} ran on a pool worker");
    }
    for prio in [0, u64::from(u32::MAX)] {
        let p = promise_new().unwrap();
        let seen: Slot<(u64, bool)> = Slot::default();
        let d = depend(
            p,
            filling(&seen, || (thread_number(), in_sync_task())),
            prio,
            true,
            false,
        );
        assert!(resolve(p, || {}));
        // it ran inside `resolve`, as a task on this thread
        assert!(is_finished(d));
        assert_eq!(seen.get(), Some(&(me, true)), "priority {prio}");
    }
    assert!(!in_sync_task());
    finish();
}

/// `await_task` in threads mode, the same rule as in the single-thread
/// scheduler: in a `sync` task a report of `GET_IN_SYNC_TASK`, then the
/// wait; elsewhere only the wait; nothing for `TaskId::FINISHED`.
#[test]
fn await_task_reports_in_a_sync_task_then_waits() {
    let _s = serial();
    start_test(2);
    let l = log();
    let reports = log();
    let pending = spawn(job(&l, "pending"), 0, false);
    let (r, l2) = (reports.clone(), l.clone());
    // a `sync` dependent of a promise this thread resolves runs here
    let p = promise_new().unwrap();
    let id = depend(
        p,
        Box::new(move || {
            await_task(TaskId::FINISHED, |m| push(&r, m));
            await_task(pending, |m| push(&r, m));
            push(&l2, "sync");
            Outcome::Done
        }),
        0,
        true,
        false,
    );
    assert!(resolve(p, || {}));
    assert!(is_finished(id));
    assert_eq!(entries(&reports), [GET_IN_SYNC_TASK]);
    assert_eq!(entries(&l), ["pending", "sync"]);
    let other = spawn(job(&l, "other"), 0, false);
    await_task(other, |m| panic!("no report outside a sync task: {m}"));
    assert!(is_finished(other));
    finish();
}

/// `IO.getTID` in threads mode is the thread's own `gettid`: a task on a
/// worker gets the worker's id, as natively.
#[cfg(feature = "io")]
#[test]
fn get_tid_is_the_threads_own() {
    let _s = serial();
    start_test(1);
    let main = crate::io::env::get_tid();
    assert_eq!(main, nix::unistd::gettid().as_raw() as u64);
    let seen: Slot<u64> = Slot::default();
    let id = spawn(filling(&seen, crate::io::env::get_tid), 0, false);
    wait(id);
    let t = *seen.get().unwrap();
    assert_ne!(t, main);
    finish();
}

#[test]
fn dependents_are_walked_newest_first_and_sync_ones_run_there() {
    let _s = serial();
    start_test(1);
    let l = log();
    let p = promise_new().unwrap();
    assert_eq!(state(p), TaskState::Running);
    let (started, go) = (Gate::default(), Gate::default());
    blocker(&l, &started, &go);
    started.wait();
    let me = thread_number();
    let ran_here = Arc::new(AtomicBool::new(false));
    let older = depend(p, job(&l, "older async"), 0, false, true);
    let r = ran_here.clone();
    let l2 = l.clone();
    let sync_dep = depend(
        p,
        Box::new(move || {
            r.store(thread_number() == me && in_sync_task(), Ordering::SeqCst);
            l2.lock().unwrap().push("sync".into());
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    let newer = depend(p, job(&l, "newer async"), 0, false, true);
    assert_eq!(state(older), TaskState::Waiting);
    assert!(resolve(p, || {}));
    // the sync one ran in `resolve`, on this thread; the others are queued
    // behind the busy worker, the newer one first
    assert!(is_finished(sync_dep));
    assert!(ran_here.load(Ordering::SeqCst));
    assert!(!resolve(p, || panic!("a second resolution stores nothing")));
    go.open();
    wait(older);
    wait(newer);
    assert_eq!(
        entries(&l),
        ["sync", "blocker", "newer async", "older async"]
    );
    finish();
}

#[test]
fn a_sync_dependent_of_a_finished_task_is_not_a_task() {
    let _s = serial();
    start_test(1);
    let s: Slot<u32> = Slot::default();
    let t = spawn(filling(&s, || 5), 0, true);
    wait(t);
    assert!(dependent_runs_now(t, true));
    assert!(!dependent_runs_now(t, false));
    // a dependent of a finished task is queued at once
    let l = log();
    let d = depend(t, job(&l, "dep"), 0, false, false);
    wait(d);
    assert_eq!(entries(&l), ["dep"]);
    // and a `sync` one made through `depend` anyway runs here at once
    let d2 = depend(TaskId::FINISHED, job(&l, "sync dep"), 0, true, false);
    assert!(is_finished(d2));
    assert_eq!(entries(&l), ["dep", "sync dep"]);
    finish();
}

#[test]
fn a_bind_task_continues_as_the_task_it_returned() {
    let _s = serial();
    start_test(2);
    let p = promise_new().unwrap();
    let pv: Slot<u32> = Slot::default();
    let out: Slot<u32> = Slot::default();
    let (pv2, out2) = (pv.clone(), out.clone());
    let ran = Gate::default();
    let ran2 = ran.clone();
    let b = spawn(
        Box::new(move || {
            ran2.open();
            Outcome::Continue(
                p,
                Box::new(move || {
                    let _ = out2.set(pv2.get().copied().unwrap_or(0) + 1);
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    // once its function has run, it waits for `p`
    ran.wait();
    until(|| state(b) == TaskState::Waiting);
    assert!(out.get().is_none());
    let pv3 = pv.clone();
    assert!(resolve(p, move || {
        let _ = pv3.set(41);
    }));
    wait(b);
    assert_eq!(out.get(), Some(&42));
    // a sync bind task whose new source has finished runs again at once
    // (a `sync` dependent of a promise this thread resolves)
    let l = log();
    let l2 = l.clone();
    let p2 = promise_new().unwrap();
    let s = depend(
        p2,
        Box::new(move || {
            Outcome::Continue(
                TaskId::FINISHED,
                Box::new(move || {
                    l2.lock().unwrap().push("continued".into());
                    Outcome::Done
                }),
            )
        }),
        0,
        true,
        false,
    );
    assert!(resolve(p2, || {}));
    assert!(is_finished(s));
    assert_eq!(entries(&l), ["continued"]);
    finish();
}

#[test]
fn a_released_pure_task_never_runs_and_its_job_is_dropped() {
    let _s = serial();
    start_test(1);
    let l = log();
    let (started, go) = (Gate::default(), Gate::default());
    blocker(&l, &started, &go);
    started.wait();
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(dropped.clone());
    let l2 = l.clone();
    let x = spawn(
        Box::new(move || {
            let _keep = &flag;
            l2.lock().unwrap().push("x ran".into());
            Outcome::Done
        }),
        0,
        false,
    );
    release(x);
    assert!(
        dropped.load(Ordering::SeqCst),
        "its job is dropped at the release"
    );
    assert!(is_finished(x));
    // an IO task released before it ran still runs
    let io = spawn(job(&l, "io ran"), 0, true);
    release(io);
    go.open();
    finish();
    assert_eq!(entries(&l), ["blocker", "io ran"]);
}

#[test]
fn releasing_a_dependent_releases_its_source() {
    let _s = serial();
    start_test(1);
    let l = log();
    let (started, go) = (Gate::default(), Gate::default());
    blocker(&l, &started, &go);
    started.wait();
    let src = spawn(job(&l, "source"), 0, false);
    /// The translator's reference to the source: its drop releases it.
    struct Handle(TaskId);
    impl Drop for Handle {
        fn drop(&mut self) {
            release(self.0);
        }
    }
    let h = Handle(src);
    let l2 = l.clone();
    let dep = depend(
        src,
        Box::new(move || {
            let _src = &h;
            l2.lock().unwrap().push("dependent".into());
            Outcome::Done
        }),
        0,
        false,
        false,
    );
    // the dependent's drop drops its source's last reference
    release(dep);
    assert!(is_finished(src));
    go.open();
    finish();
    assert_eq!(entries(&l), ["blocker"]);
}

#[test]
fn a_running_pure_task_released_is_canceled_and_finishes() {
    let _s = serial();
    start_test(1);
    let (started, go) = (Gate::default(), Gate::default());
    let seen: Slot<(bool, bool)> = Slot::default();
    let (s2, g2) = (started.clone(), go.clone());
    let t = spawn(
        filling(&seen, move || {
            let before = check_canceled();
            s2.open();
            g2.wait();
            (before, check_canceled())
        }),
        0,
        false,
    );
    started.wait();
    release(t);
    go.open();
    finish();
    assert_eq!(seen.get(), Some(&(false, true)));
}

#[test]
fn cancellation_reaches_dependents() {
    let _s = serial();
    start_test(2);
    let p = promise_new().unwrap();
    let seen: Slot<bool> = Slot::default();
    let d = depend(p, filling(&seen, check_canceled), 0, false, true);
    let late_seen: Slot<bool> = Slot::default();
    cancel(p);
    assert!(resolve(p, || {}));
    wait(d);
    assert_eq!(seen.get(), Some(&true));
    // a dependent made after the finish is not canceled
    let d2 = depend(p, filling(&late_seen, check_canceled), 0, false, true);
    wait(d2);
    assert_eq!(late_seen.get(), Some(&false));
    // canceling a running task: its own check sees it
    let (started, go) = (Gate::default(), Gate::default());
    let own: Slot<bool> = Slot::default();
    let (s2, g2) = (started.clone(), go.clone());
    let t = spawn(
        filling(&own, move || {
            s2.open();
            g2.wait();
            check_canceled()
        }),
        0,
        true,
    );
    started.wait();
    cancel(t);
    go.open();
    wait(t);
    assert_eq!(own.get(), Some(&true));
    assert!(!check_canceled(), "false outside tasks");
    finish();
}

#[test]
fn wait_any_takes_the_first_finished_in_list_order() {
    let _s = serial();
    start_test(2);
    let p = promise_new().unwrap();
    let q = promise_new().unwrap();
    let s: Slot<u32> = Slot::default();
    let t = spawn(filling(&s, || 1), 0, true);
    wait(t);
    assert_eq!(wait_any(&[p, t, TaskId::FINISHED]), 1);
    assert_eq!(wait_any(&[TaskId::FINISHED, t]), 0);
    // none finished: it blocks until one finishes
    let waiter: Slot<usize> = Slot::default();
    let w = spawn(filling(&waiter, move || wait_any(&[p, q])), 9, true);
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(waiter.get().is_none());
    assert!(resolve(q, || {}));
    wait(w);
    assert_eq!(waiter.get(), Some(&1));
    assert!(resolve(p, || {}));
    finish();
}

#[test]
fn a_pool_task_that_waits_frees_its_place_but_wait_any_keeps_it() {
    let _s = serial();
    let sh = start_test(1);
    // `wait`: the pool grows by one, so the task that resolves the promise
    // runs although the only worker waits (else a deadlock)
    let p = promise_new().unwrap();
    let a = spawn(
        Box::new(move || {
            wait(p);
            Outcome::Done
        }),
        0,
        true,
    );
    let b = spawn(
        Box::new(move || {
            resolve(p, || {});
            Outcome::Done
        }),
        0,
        true,
    );
    wait(a);
    wait(b);
    assert_eq!(live_workers(&sh), 2);
    // `wait_any`: the waiter keeps its place in the pool (case
    // `tasks/wait_any_keeps_worker`): with the limit of one, the queued task
    // runs only after it, although the second worker is idle
    let l = log();
    let q = promise_new().unwrap();
    let la = l.clone();
    let t = spawn(
        Box::new(move || {
            wait_any(&[q]);
            la.lock().unwrap().push("T done".into());
            Outcome::Done
        }),
        0,
        true,
    );
    until(|| state(t) == TaskState::Running);
    let after = spawn(job(&l, "B"), 0, true);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(state(after), TaskState::Waiting);
    assert_eq!(live_workers(&sh), 2, "waitAny made no worker");
    assert!(resolve(q, || {}));
    wait(t);
    wait(after);
    assert_eq!(entries(&l), ["T done", "B"]);
    finish();
}

#[test]
fn a_task_enqueued_after_main_returned_runs() {
    // LB-13: natively a pool task enqueued once no standard worker is left
    // never runs, and a wait on it hangs; here it runs, and the exit waits.
    let _s = serial();
    let sh = start_test(1);
    let l = log();
    let l2 = l.clone();
    let seen: Slot<bool> = Slot::default();
    let s2 = seen.clone();
    let d = spawn(
        Box::new(move || {
            // `main` is in `finish` by now, with no worker left
            std::thread::sleep(std::time::Duration::from_millis(20));
            let l3 = l2.clone();
            let s3 = s2.clone();
            let late = spawn(
                Box::new(move || {
                    let _ = s3.set(check_canceled());
                    l3.lock().unwrap().push("late pool task".into());
                    Outcome::Done
                }),
                0,
                true,
            );
            wait(late);
            l2.lock().unwrap().push("dedicated done".into());
            Outcome::Done
        }),
        9,
        true,
    );
    // a dependent of the dedicated task, queued when it finishes
    let dep = depend(d, job(&l, "late dependent"), 0, false, true);
    finish();
    assert!(is_finished(dep));
    assert_eq!(
        entries(&l),
        ["late pool task", "dedicated done", "late dependent"]
    );
    // the shutdown flag was set when the late task ran
    assert_eq!(seen.get(), Some(&true));
    assert_eq!(live_workers(&sh), 0);
}

#[test]
fn the_exit_waits_for_queued_and_running_tasks_but_not_for_promises() {
    let _s = serial();
    let sh = start_test(1);
    let l = log();
    let p = promise_new().unwrap();
    // waits for a promise not resolved before the exit: not waited for
    let never = depend(p, job(&l, "never"), 0, false, true);
    for k in 0..3 {
        spawn(job(&l, &format!("io {k}")), 0, true);
    }
    spawn(job(&l, "referenced pure"), 0, false);
    finish();
    assert_eq!(entries(&l), ["io 0", "io 1", "io 2", "referenced pure"]);
    assert!(!is_finished(never));
    // the promise and its dependent stay
    assert_eq!(table_len(&sh), 2);
    // resolved afterwards (a translator's value dropped at exit), with no
    // task manager left: its dependent runs here, at once
    assert!(resolve(p, || {}));
    assert!(is_finished(never));
    assert_eq!(table_len(&sh), 0);
    assert_eq!(entries(&l).last().map(String::as_str), Some("never"));
    assert_eq!(live_workers(&sh), 0);
}

#[test]
fn concurrent_resolutions_store_once() {
    let _s = serial();
    start_test(4);
    let p = promise_new().unwrap();
    let stores = Arc::new(AtomicU32::new(0));
    let wins = Arc::new(AtomicU32::new(0));
    let seen_finished = Arc::new(AtomicU32::new(0));
    let ids: Vec<TaskId> = (0..4)
        .map(|_| {
            let (st, w, f) = (stores.clone(), wins.clone(), seen_finished.clone());
            spawn(
                Box::new(move || {
                    let won = resolve(p, || {
                        st.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    });
                    if won {
                        w.fetch_add(1, Ordering::SeqCst);
                    }
                    // after any resolution has returned, the value is set
                    if is_finished(p) {
                        f.fetch_add(1, Ordering::SeqCst);
                    }
                    Outcome::Done
                }),
                0,
                true,
            )
        })
        .collect();
    for id in ids {
        wait(id);
    }
    assert_eq!(stores.load(Ordering::SeqCst), 1);
    assert_eq!(wins.load(Ordering::SeqCst), 1);
    assert_eq!(seen_finished.load(Ordering::SeqCst), 4);
    finish();
}

#[test]
fn lb32_wakes_the_waiters_of_a_walk_that_never_ends() {
    // A task's waiters natively wake only after its walk of dependents; a
    // `sync` dependent that blocks holds them. `option_get_or_block`'s wake
    // (here called directly, as its permanent block would hang the test)
    // lets the waiters of finished tasks return.
    let _s = serial();
    let sh = start_test(2);
    let p = promise_new().unwrap();
    let (in_walk, go) = (Gate::default(), Gate::default());
    let (iw, g2) = (in_walk.clone(), go.clone());
    let blocked = depend(
        p,
        Box::new(move || {
            iw.open();
            g2.wait();
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    let woke = Arc::new(AtomicBool::new(false));
    let w2 = woke.clone();
    let waiter = spawn(
        Box::new(move || {
            wait(p);
            w2.store(true, Ordering::SeqCst);
            Outcome::Done
        }),
        9,
        true,
    );
    until(|| state(waiter) == TaskState::Running);
    // resolve on a thread of its own: its walk blocks in the `sync`
    // dependent
    let resolver = spawn(
        Box::new(move || {
            resolve(p, || {});
            Outcome::Done
        }),
        9,
        true,
    );
    in_walk.wait();
    assert!(is_finished(p));
    wake_waiters(&sh);
    until(|| woke.load(Ordering::SeqCst));
    go.open();
    wait(resolver);
    wait(blocked);
    wait(waiter);
    finish();
}

#[test]
#[cfg_attr(miri, ignore)]
fn option_get_or_block_reports_wakes_and_blocks_its_thread() {
    // The whole path, with the thread that drops the promise blocked for
    // good afterwards (as natively): the test ends without `finish`, and
    // leaves that thread parked (not under Miri, which wants every thread
    // joined).
    let _s = serial();
    start_test(2);
    let p = promise_new().unwrap();
    let opt: Slot<Option<u32>> = Slot::default();
    let reported = Arc::new(AtomicBool::new(false));
    let (o2, r2) = (opt.clone(), reported.clone());
    let bang: Slot<u32> = Slot::default();
    depend(
        p,
        filling(&bang, move || {
            option_get_or_block(o2.get().copied().flatten(), |m| {
                assert_eq!(m, PROMISE_DROPPED);
                r2.store(true, Ordering::SeqCst);
            })
        }),
        0,
        true,
        false,
    );
    let woke = Arc::new(AtomicBool::new(false));
    let w2 = woke.clone();
    let o3 = opt.clone();
    let waiter = spawn(
        Box::new(move || {
            wait(p);
            w2.store(o3.get() == Some(&None), Ordering::SeqCst);
            Outcome::Done
        }),
        9,
        true,
    );
    until(|| state(waiter) == TaskState::Running);
    // the promise's last reference goes on a thread of its own
    let o4 = opt.clone();
    spawn(
        Box::new(move || {
            resolve(p, move || {
                let _ = o4.set(None);
            });
            Outcome::Done
        }),
        9,
        true,
    );
    until(|| woke.load(Ordering::SeqCst));
    assert!(reported.load(Ordering::SeqCst));
    assert!(bang.get().is_none());
}

#[test]
fn check_canceled_at_shutdown() {
    let _s = serial();
    start_test(1);
    let n = Arc::new(AtomicU32::new(0));
    let n2 = n.clone();
    let s2 = Gate::default();
    let s3 = s2.clone();
    spawn(
        Box::new(move || {
            s3.open();
            while !check_canceled() {
                n2.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Outcome::Done
        }),
        0,
        true,
    );
    s2.wait();
    // `finish` sets the flag; the task sees it and ends, so `finish` returns
    finish();
    assert!(n.load(Ordering::Relaxed) >= 1);
}

/// A glue that logs its hooks.
struct LogGlue(Log);

impl Glue for LogGlue {
    fn thread_start(&self) {
        push(&self.0, "thread start");
    }
    fn thread_end(&self) {
        push(&self.0, "thread end");
    }
    fn task_begin(&self, own_thread: bool) {
        push(
            &self.0,
            if own_thread {
                "begin own"
            } else {
                "begin sync"
            },
        );
    }
    fn task_end(&self, own_thread: bool) {
        push(&self.0, if own_thread { "end own" } else { "end sync" });
    }
}

#[test]
fn the_glue_hooks_pair_up() {
    // A pool task on a worker of its own, and its `sync` dependent in its
    // walk on the same thread: the dependent's hooks nest in the task's,
    // whose `task_end` comes after its walk.
    let _s = serial();
    let sh = bind_local();
    let l = log();
    configure(&sh, Arc::new(LogGlue(l.clone())), 1, 256 << 10);
    let go = Gate::default();
    let g2 = go.clone();
    let t = spawn(
        Box::new(move || {
            g2.wait();
            Outcome::Done
        }),
        0,
        true,
    );
    let d = depend(t, job(&l, "sync dependent"), 0, true, true);
    go.open();
    wait(d);
    finish();
    assert_eq!(
        entries(&l),
        [
            "thread start",
            "begin own",
            "begin sync",
            "sync dependent",
            "end sync",
            "end own",
            "thread end"
        ]
    );
}

/// A glue that holds the first `task_end` of one kind (of a task on a
/// thread of its own, or of a `sync` task): it opens `in_hook`, then waits
/// for `go`, at most `limit`.
struct HoldEndGlue {
    own: bool,
    held: AtomicBool,
    in_hook: Gate,
    go: Gate,
    limit: std::time::Duration,
}

impl HoldEndGlue {
    fn new(own: bool, limit: std::time::Duration) -> HoldEndGlue {
        HoldEndGlue {
            own,
            held: AtomicBool::new(false),
            in_hook: Gate::default(),
            go: Gate::default(),
            limit,
        }
    }
}

impl Glue for HoldEndGlue {
    fn task_end(&self, own_thread: bool) {
        if own_thread == self.own && !self.held.swap(true, Ordering::SeqCst) {
            self.in_hook.open();
            self.go.wait_at_most(self.limit);
        }
    }
}

/// Natively a worker counts itself idle (`m_idle_std_workers++`) under the
/// lock it has held since its task's closure returned, through
/// `resolve_core`: a thread that sees the task finished and then queues a
/// task finds that worker idle, and the task goes to it (`enqueue_core`
/// makes a worker only when none is idle; `tasks/worker_keeps_streams`).
/// Here the glue's `task_end` runs after the walk, outside the lock, so the
/// worker counts itself idle before it. The hook holds that gap open until
/// `main` has queued `b`. Before the fix the worker counted itself idle
/// only after the hook: `b`'s enqueue made a second worker.
#[test]
fn a_worker_is_idle_in_its_tasks_task_end() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(true, std::time::Duration::from_secs(60)));
    configure(&sh, glue.clone(), 4, 256 << 10);
    let a: Slot<Option<u32>> = Slot::default();
    let ta = spawn(filling(&a, running_worker), 0, true);
    // `a` has finished, and its worker is in its `task_end`
    glue.in_hook.wait();
    assert!(is_finished(ta));
    let b: Slot<Option<u32>> = Slot::default();
    let tb = spawn(filling(&b, running_worker), 0, true);
    let live = live_workers(&sh);
    glue.go.open();
    wait(tb);
    finish();
    assert_eq!(live, 1, "b's enqueue found a's worker idle: no new worker");
    assert_eq!(a.get(), Some(&Some(0)));
    assert_eq!(b.get(), a.get(), "b ran on a's worker");
}

/// The same gap after a `sync` dependent, the last task of its source's
/// walk: natively its `resolve_core` notifies under the lock, which the
/// worker then keeps until it is idle, so the waiter it wakes finds the
/// worker idle. Here the dependent's `task_end` runs outside the lock, as
/// part of its run, before its waiters are notified. The hook waits for
/// `main` at most 300 ms: `main` sleeps in `wait(s)` until the hook has
/// ended. Before the fix the notification came first: `main` went on in
/// the hook and queued `b` while `a`'s worker was busy, which made a second
/// worker.
#[test]
fn a_sync_dependents_task_end_comes_before_its_waiters_wake() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(
        false,
        std::time::Duration::from_millis(300),
    ));
    configure(&sh, glue.clone(), 4, 256 << 10);
    let a: Slot<Option<u32>> = Slot::default();
    let ta = spawn(filling(&a, running_worker), 0, true);
    let sh2 = sh.clone();
    // `s`, walked on `a`'s worker, ends once `main` sleeps in `wait(s)`
    let ts = depend(
        ta,
        Box::new(move || {
            until(|| blocked_waits(&sh2) == 1);
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    wait(ts);
    let b: Slot<Option<u32>> = Slot::default();
    let tb = spawn(filling(&b, running_worker), 0, true);
    let live = live_workers(&sh);
    glue.go.open();
    wait(tb);
    finish();
    assert_eq!(live, 1, "b's enqueue found a's worker idle: no new worker");
    assert_eq!(a.get(), Some(&Some(0)));
    assert_eq!(b.get(), a.get(), "b ran on a's worker");
}

/// Natively a task's standard streams (`IO.setStdout` & co.) and `errno`
/// are its thread's (`io.cpp` 115-117, `MK_THREAD_LOCAL_GET`; review
/// AR-24): a pool worker keeps them from one task to the next, and a new
/// worker, a dedicated task's thread and `main` start with the process's
/// streams and `errno` 0. Threads mode gets that from its real threads, with
/// no glue hook: `main`'s redirection is not the tasks'; a task's
/// redirection and `errno` reach its `sync` dependent, walked on its thread,
/// and the next task of the same (only) worker; a dedicated task starts
/// fresh; `main` keeps its own.
#[cfg(feature = "io")]
#[test]
fn a_pool_worker_keeps_its_streams_and_errno() {
    use crate::io::error::{errno, set_errno};
    use crate::io::streams::{self, StdStream};
    fn out() -> u32 {
        streams::current(StdStream::Stdout, || 0u32)
    }
    let _s = serial();
    start_test(1);
    assert_eq!(streams::set_stdout(7u32, || 0), 0);
    set_errno(9);
    let go = Gate::default();
    let g2 = go.clone();
    let a: Slot<(u32, i32)> = Slot::default();
    let ta = spawn(
        filling(&a, move || {
            g2.wait();
            let seen = (out(), errno());
            let _ = streams::set_stdout(5u32, || 0);
            set_errno(2);
            seen
        }),
        0,
        true,
    );
    // walked on `a`'s worker, after its job
    let d: Slot<(u32, i32)> = Slot::default();
    let td = depend(ta, filling(&d, || (out(), errno())), 0, true, true);
    go.open();
    wait(td);
    // the only worker, after `a`
    let b: Slot<(u32, i32)> = Slot::default();
    let tb = spawn(filling(&b, || (out(), errno())), 0, true);
    let c: Slot<(u32, i32)> = Slot::default();
    let tc = spawn(filling(&c, || (out(), errno())), 9, true);
    wait(tb);
    wait(tc);
    finish();
    assert_eq!(a.get(), Some(&(0, 0)), "a new worker starts fresh");
    assert_eq!(d.get(), Some(&(5, 2)), "a sync dependent shares the thread");
    assert_eq!(b.get(), Some(&(5, 2)), "the worker kept them");
    assert_eq!(
        c.get(),
        Some(&(0, 0)),
        "a dedicated task's thread starts fresh"
    );
    assert_eq!((out(), errno()), (7, 9), "main keeps its own");
    assert_eq!(streams::set_stdout(0u32, || 0), 7);
}

thread_local! {
    /// A glue's own per-task state (lean2rr's stream context), for the tests
    /// of `end_running_task`.
    static GLUE_CTX: Cell<&'static str> = const { Cell::new("a fresh thread's") };
}

/// The glue's protocol of review AR-26 (lean2rr's AR-S1) for a job whose
/// task id `id` holds once it runs: open its own context (`name`), store the
/// value, `end_running_task(id)` (unless `end_first` is false), close the
/// context.
fn protocol_job(name: &'static str, id: Slot<TaskId>, end_first: bool) -> Job {
    Box::new(move || {
        GLUE_CTX.with(|c| c.set(name));
        if end_first {
            end_running_task(*id.get().expect("the task's id"));
        }
        GLUE_CTX.with(|c| c.set("closed"));
        Outcome::Done
    })
}

/// A pool task whose job waits for `go`, then runs `job` (so the caller can
/// store the returned id where the job reads it before it runs).
fn gated(go: &Gate, job: Job) -> TaskId {
    let g2 = go.clone();
    spawn(
        Box::new(move || {
            g2.wait();
            job()
        }),
        0,
        true,
    )
}

/// Review AR-26 (lean2rr's AR-S1): a job that opens its own context (a
/// translator's stream context), stores the task's value, calls
/// `end_running_task` and then closes the context: the task's `sync`
/// dependent runs on the task's thread inside the context, its waiter wakes,
/// and the glue's `task_end` comes once, after. A job that does not call it
/// (the control) has its dependent run after the context closed.
#[test]
fn a_job_ends_its_task_before_it_closes_its_context() {
    let _s = serial();
    let sh = bind_local();
    let l = log();
    configure(&sh, Arc::new(LogGlue(l.clone())), 1, 256 << 10);
    let mut seen = Vec::new();
    for end_first in [true, false] {
        let id: Slot<TaskId> = Slot::default();
        let go = Gate::default();
        let a = gated(&go, protocol_job("the task's", id.clone(), end_first));
        let _ = id.set(a);
        let s: Slot<&'static str> = Slot::default();
        let d = depend(a, filling(&s, || GLUE_CTX.with(Cell::get)), 0, true, true);
        go.open();
        wait(d);
        seen.push(*s.get().unwrap());
    }
    finish();
    assert_eq!(seen, ["the task's", "closed"]);
    let hooks = entries(&l);
    assert_eq!(
        hooks.iter().filter(|h| *h == "end own").count(),
        2,
        "one task_end per task: {hooks:?}"
    );
}

/// Review RT2-14: `end_running_task(id)` ends task `id` only while this
/// thread runs its job. A job that A's job runs itself (the glue's inline
/// path: `TaskId::FINISHED`), and a second call, end nothing: A's `sync`
/// dependent sees the value A stores after them. Before the fix the call
/// ended the innermost running task, A, before A stored its value.
#[test]
fn rt2_14_end_running_task_ends_only_its_own_task() {
    let _s = serial();
    let sh = bind_local();
    configure(&sh, Arc::new(TestGlue), 1, 256 << 10);
    let value: Slot<u32> = Slot::default();
    let seen: Slot<Option<u32>> = Slot::default();
    let id: Slot<TaskId> = Slot::default();
    let (v2, id2) = (value.clone(), id.clone());
    let go = Gate::default();
    let a = gated(
        &go,
        Box::new(move || {
            let inner: Job = Box::new(|| {
                end_running_task(TaskId::FINISHED);
                Outcome::Done
            });
            let _ = inner();
            let _ = v2.set(42);
            end_running_task(*id2.get().unwrap());
            end_running_task(*id2.get().unwrap());
            Outcome::Done
        }),
    );
    let _ = id.set(a);
    let v3 = value.clone();
    let d = depend(a, filling(&seen, move || v3.get().copied()), 0, true, true);
    go.open();
    wait(d);
    finish();
    assert_eq!(
        seen.get(),
        Some(&Some(42)),
        "A's dependent ran before A stored its value"
    );
}

/// Review RT2-15 (leanrs's AR26-01), threads mode: in a chain A -> B -> C of
/// `sync` dependents, B (walked by A's thread) follows the protocol: C runs
/// inside B's context, as in the single-thread scheduler and natively.
#[test]
fn rt2_15_end_running_task_in_a_walked_sync_dependent() {
    let _s = serial();
    let sh = bind_local();
    configure(&sh, Arc::new(TestGlue), 1, 256 << 10);
    let (ida, idb): (Slot<TaskId>, Slot<TaskId>) = (Slot::default(), Slot::default());
    let go = Gate::default();
    let a = gated(&go, protocol_job("A's", ida.clone(), true));
    let _ = ida.set(a);
    let b = depend(a, protocol_job("B's", idb.clone(), true), 0, true, true);
    let _ = idb.set(b);
    let seen: Slot<&'static str> = Slot::default();
    let c = depend(
        b,
        filling(&seen, || GLUE_CTX.with(Cell::get)),
        0,
        true,
        true,
    );
    go.open();
    wait(c);
    finish();
    assert_eq!(seen.get(), Some(&"B's"));
}

/// The child process of `rt2_16_continue_after_end_running_task_aborts`.
const RT2_16_CHILD: &str = "LEAN_RUNTIME_TEST_RT2_16_CHILD";

/// Review RT2-16: a job that returns `Continue` after `end_running_task`
/// aborts the process with the crate's message. Before the fix it panicked
/// after its abort guard, with the scheduler's lock held: the worker ended
/// while `live` still counted it, and `finish` waited for good (RT1-01's
/// failure). In a child process, killed after 10 s.
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_16_continue_after_end_running_task_aborts() {
    if std::env::var_os(RT2_16_CHILD).is_none() {
        let _s = serial();
        let t0 = std::time::Instant::now();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "sched::mt::tests::rt2_16_continue_after_end_running_task_aborts",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(RT2_16_CHILD, "1")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let st = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            if t0.elapsed() > std::time::Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child hangs (finish waits for the dead worker)");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let mut err = String::new();
        use std::io::Read;
        let _ = child.stderr.take().unwrap().read_to_string(&mut err);
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(st.signal(), Some(6), "an abort; stderr:\n{err}");
        assert!(
            err.contains("end_running_task"),
            "the message; stderr:\n{err}"
        );
        return;
    }
    let sh = bind_local();
    configure(&sh, Arc::new(TestGlue), 1, 256 << 10);
    let src: Slot<u32> = Slot::default();
    let s = spawn(filling(&src, || 1), 0, true);
    wait(s);
    let id: Slot<TaskId> = Slot::default();
    let id2 = id.clone();
    let go = Gate::default();
    let a = gated(
        &go,
        Box::new(move || {
            end_running_task(*id2.get().unwrap());
            Outcome::Continue(s, Box::new(|| Outcome::Done))
        }),
    );
    let _ = id.set(a);
    go.open();
    std::thread::sleep(std::time::Duration::from_millis(200));
    finish();
}

#[test]
fn no_suspend_scope_is_counted_per_thread() {
    assert!(!in_no_suspend());
    enter_no_suspend();
    assert!(in_no_suspend());
    {
        let _g = no_suspend();
        assert!(in_no_suspend());
        std::thread::spawn(|| assert!(!in_no_suspend()))
            .join()
            .unwrap();
    }
    leave_no_suspend();
    assert!(!in_no_suspend());
    leave_no_suspend();
    assert!(!in_no_suspend());
    assert!(!io_cooperative() && !coop_possible());
    assert!(running_stack().is_none());
}

// ---------------------------------------------------------------------------
// Std.Sync under real contention

#[test]
fn a_mutex_excludes_and_hands_over() {
    let _s = serial();
    start_test(4);
    let m = Arc::new(Mutex::new());
    let inside = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicU32::new(0));
    let ids: Vec<TaskId> = (0..4)
        .map(|_| {
            let (m, inside, count) = (m.clone(), inside.clone(), count.clone());
            spawn(
                Box::new(move || {
                    for _ in 0..25 {
                        m.lock();
                        assert!(!inside.swap(true, Ordering::SeqCst), "two threads inside");
                        // a read and a write, not one atomic step: an
                        // update lost without the lock would show
                        let c = count.load(Ordering::Relaxed);
                        std::hint::spin_loop();
                        count.store(c + 1, Ordering::Relaxed);
                        inside.store(false, Ordering::SeqCst);
                        m.unlock();
                    }
                    Outcome::Done
                }),
                0,
                true,
            )
        })
        .collect();
    for id in ids {
        wait(id);
    }
    assert_eq!(count.load(Ordering::SeqCst), 100);
    assert!(m.try_lock());
    assert!(!m.try_lock());
    m.unlock();
    finish();
}

#[test]
fn a_condvar_wait_returns_once_notified() {
    let _s = serial();
    start_test(2);
    let m = Arc::new(Mutex::new());
    let cv = Arc::new(Condvar::new());
    // items under `m`: produced and consumed counts
    let produced = Arc::new(AtomicU32::new(0));
    let (m2, cv2, p2) = (m.clone(), cv.clone(), produced.clone());
    let producer = spawn(
        Box::new(move || {
            for _ in 0..20 {
                m2.lock();
                p2.store(p2.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                cv2.notify_one();
                m2.unlock();
            }
            Outcome::Done
        }),
        0,
        true,
    );
    let mut consumed = 0;
    m.lock();
    while consumed < 20 {
        while produced.load(Ordering::Relaxed) == consumed {
            cv.wait(&m);
        }
        consumed = produced.load(Ordering::Relaxed);
    }
    m.unlock();
    wait(producer);
    // notify_all wakes every waiter
    let woken = Arc::new(AtomicU32::new(0));
    let flag = Arc::new(AtomicBool::new(false));
    let ids: Vec<TaskId> = (0..2)
        .map(|_| {
            let (m, cv, w, f) = (m.clone(), cv.clone(), woken.clone(), flag.clone());
            spawn(
                Box::new(move || {
                    m.lock();
                    while !f.load(Ordering::Relaxed) {
                        cv.wait(&m);
                    }
                    w.fetch_add(1, Ordering::SeqCst);
                    m.unlock();
                    Outcome::Done
                }),
                9,
                true,
            )
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(5));
    m.lock();
    flag.store(true, Ordering::Relaxed);
    cv.notify_all();
    m.unlock();
    for id in ids {
        wait(id);
    }
    assert_eq!(woken.load(Ordering::SeqCst), 2);
    finish();
}

#[test]
fn a_recursive_mutex_counts_its_owner() {
    let _s = serial();
    start_test(3);
    let m = Arc::new(RecursiveMutex::new());
    let inside = Arc::new(AtomicUsize::new(0));
    let ids: Vec<TaskId> = (0..3)
        .map(|_| {
            let (m, inside) = (m.clone(), inside.clone());
            spawn(
                Box::new(move || {
                    for _ in 0..10 {
                        m.lock();
                        assert!(m.try_lock());
                        m.lock();
                        assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
                        inside.fetch_sub(1, Ordering::SeqCst);
                        m.unlock();
                        m.unlock();
                        m.unlock();
                    }
                    Outcome::Done
                }),
                0,
                true,
            )
        })
        .collect();
    for id in ids {
        wait(id);
    }
    // free again: another thread takes it
    let t = spawn(
        Box::new({
            let m = m.clone();
            move || {
                assert!(m.try_lock());
                m.unlock();
                Outcome::Done
            }
        }),
        9,
        true,
    );
    wait(t);
    finish();
}

/// AR-39 (lean2rr's review RS7-02): a lock's owner is the OS thread. An
/// initializer keeps a recursive mutex locked before the task manager runs:
/// `main` on the same OS thread (`LEAN_MAIN_USE_THREAD=0`) locks it again
/// after `start`, and another OS thread (`main` on a thread of its own)
/// cannot take it, with workers and with none, as natively. Before AR-39 the
/// owner also held whether the task manager ran, so with workers the
/// relock on the same thread waited for good.
#[test]
fn a_recursive_lock_is_the_os_threads_across_the_start() {
    let _s = serial();
    for workers in [2, 0] {
        std::thread::spawn(move || {
            // the initializers
            let m = Arc::new(RecursiveMutex::new());
            m.lock();
            // `main`, on the initializers' thread
            start_test(workers);
            assert!(m.try_lock(), "workers {workers}");
            m.lock();
            // `main` on a thread of its own
            let other = m.clone();
            std::thread::spawn(move || assert!(!other.try_lock(), "workers {workers}"))
                .join()
                .unwrap();
            for _ in 0..3 {
                m.unlock();
            }
            let other = m.clone();
            std::thread::spawn(move || {
                assert!(other.try_lock(), "workers {workers}");
                other.unlock();
            })
            .join()
            .unwrap();
            finish();
        })
        .join()
        .unwrap();
    }
}

#[test]
fn a_shared_mutex_lets_readers_share_and_writers_exclude() {
    let _s = serial();
    start_test(3);
    let m = Arc::new(SharedMutex::new());
    // three readers inside at once
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let readers: Vec<TaskId> = (0..3)
        .map(|_| {
            let (m, b) = (m.clone(), barrier.clone());
            spawn(
                Box::new(move || {
                    m.read();
                    b.wait();
                    m.unlock_read();
                    Outcome::Done
                }),
                9,
                true,
            )
        })
        .collect();
    for id in readers {
        wait(id);
    }
    // a writer excludes readers and other writers
    m.write();
    assert!(!m.try_read() && !m.try_write());
    let inside = Arc::new(AtomicBool::new(false));
    let (m2, i2) = (m.clone(), inside.clone());
    let r = spawn(
        Box::new(move || {
            m2.read();
            i2.store(true, Ordering::SeqCst);
            m2.unlock_read();
            Outcome::Done
        }),
        9,
        true,
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(!inside.load(Ordering::SeqCst));
    m.unlock_write();
    wait(r);
    assert!(inside.load(Ordering::SeqCst));
    // readers in, a writer waits for them to leave
    m.read();
    let wrote = Arc::new(AtomicBool::new(false));
    let (m3, w3) = (m.clone(), wrote.clone());
    let w = spawn(
        Box::new(move || {
            m3.write();
            w3.store(true, Ordering::SeqCst);
            m3.unlock_write();
            Outcome::Done
        }),
        9,
        true,
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(!wrote.load(Ordering::SeqCst));
    m.unlock_read();
    wait(w);
    assert!(wrote.load(Ordering::SeqCst));
    finish();
}

// ---------------------------------------------------------------------------
// Refs: Lean 4.35's rule (LB-01, LB-18)

/// `modify` holds the reference while its function waits for a thread that
/// was told the reference is taken; what that thread's operation sees and
/// leaves.
fn during_modify<R: Send + Sync + 'static>(
    op: impl FnOnce(&Ref<u32>) -> R + Send + 'static,
) -> (R, u32) {
    let r = Arc::new(Ref::new(0u32));
    let taken = Gate::default();
    let out: Slot<R> = Slot::default();
    let (r2, t2, o2) = (r.clone(), taken.clone(), out.clone());
    let other = spawn(
        Box::new(move || {
            t2.wait();
            let _ = o2.set(op(&r2));
            Outcome::Done
        }),
        9,
        true,
    );
    r.modify(|v| {
        taken.open();
        // the other thread's operation now waits for this store: long
        // enough for that thread to reach its operation under load too (5
        // ms flaked once at a load of about 31)
        std::thread::sleep(std::time::Duration::from_millis(100));
        v + 1
    });
    wait(other);
    let left = r.get();
    match Arc::try_unwrap(out) {
        Ok(o) => (o.into_inner().expect("a result"), left),
        Err(_) => panic!("the slot is still shared"),
    }
}

#[test]
fn a_set_during_modify_is_not_lost() {
    // `refs/set_during_modify` (LB-01): 4.34.0 ends with 1; 4.35, here, 100
    let _s = serial();
    start_test(1);
    assert_eq!(during_modify(|r| r.set(100)), ((), 100));
    finish();
}

#[test]
fn a_swap_during_modify_returns_the_stored_value() {
    // `refs/swap_during_modify` (LB-18): 4.34.0's swap returns 100 itself
    let _s = serial();
    start_test(1);
    assert_eq!(during_modify(|r| r.swap(100)), (1, 100));
    finish();
}

#[test]
fn a_get_during_modify_waits_for_its_store() {
    // `refs/get_during_modify`
    let _s = serial();
    start_test(1);
    assert_eq!(during_modify(|r| r.get()), (1, 1));
    finish();
}

#[test]
fn a_completed_set_is_seen_by_every_later_get() {
    // `refs/lost_update` (LB-01): a task sets once while `main` reads
    let _s = serial();
    start_test(1);
    let r = Arc::new(Ref::new(0u32));
    let r2 = r.clone();
    let t = spawn(
        Box::new(move || {
            r2.set(1);
            Outcome::Done
        }),
        0,
        true,
    );
    while !is_finished(t) {
        let _ = r.get();
    }
    wait(t);
    assert_eq!(r.get(), 1);
    // modify and modify_get are atomic against concurrent ones
    let ids: Vec<TaskId> = (0..3)
        .map(|_| {
            let r = r.clone();
            spawn(
                Box::new(move || {
                    for _ in 0..10 {
                        r.modify(|v| v + 1);
                        let _: u32 = r.modify_get(|v| (v, v + 1));
                    }
                    Outcome::Done
                }),
                9,
                true,
            )
        })
        .collect();
    for id in ids {
        wait(id);
    }
    assert_eq!(r.get(), 61);
    assert_eq!(r.take(), 61);
    r.put(5);
    assert_eq!(r.get(), 5);
    finish();
}

/// `empty()`: a placeholder that well-typed code never reads; a `put`
/// fills it, as the single-thread `sched::Ref`'s.
#[test]
fn an_empty_reference_is_filled_by_put() {
    let r: Ref<u32> = Ref::empty();
    r.put(3);
    assert_eq!(r.get(), 3);
    r.set(4);
    assert_eq!(r.swap(5), 4);
    assert_eq!(r.take(), 5);
}

// ---------------------------------------------------------------------------
// The deferred resolutions of a translator's drains (wait-1, core 3.3): the
// bodies of `sched/drain.rs`, also run by the single-thread scheduler's
// tests. A `sync` dependent runs on the resolving thread, the test's.

macro_rules! drain_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let _s = serial();
                start_test(1);
                crate::sched::drain::tests::$name();
                finish();
            }
        )*
    };
}

drain_tests!(
    resolutions_run_after_the_drain_in_drop_order,
    a_nested_drain_resolves_its_own_promises_first,
    entries_moved_out_still_count_as_pending,
    a_panic_through_a_drain_leaves_the_entries_queued,
    a_panic_out_of_an_entry_requeues_the_rest,
    run_deferred_inside_a_scope,
    resolve_inside_a_scope,
    scopes_nest,
);

// ---------------------------------------------------------------------------
// The stack-overflow report on the manager's threads (leanrs's point 4)

#[cfg(feature = "stack-overflow")]
#[test]
#[cfg_attr(miri, ignore)]
fn every_thread_the_manager_makes_registers_with_the_report() {
    let _s = serial();
    start_test(1);
    let pool: Slot<bool> = Slot::default();
    let dedicated: Slot<bool> = Slot::default();
    let a = spawn(
        filling(&pool, crate::sched::stack_overflow::registered),
        0,
        true,
    );
    let b = spawn(
        filling(&dedicated, crate::sched::stack_overflow::registered),
        9,
        true,
    );
    wait(a);
    wait(b);
    assert_eq!(pool.get(), Some(&true));
    assert_eq!(dedicated.get(), Some(&true));
    finish();
}

// ---------------------------------------------------------------------------
// The review of T1 (RT1): regression tests of its repros

/// Panics when dropped: a translator's value whose destructor panics.
struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("a translator destructor panics");
    }
}

/// The child process of `rt1_01_a_panic_in_a_dropped_continuation_aborts`.
const RT1_01_CHILD: &str = "LEAN_RUNTIME_TEST_RT1_01_CHILD";

/// RT1-01 (the review's `rt1_panic_in_dropped_continuation_wedges_the_pool`):
/// a pure bind task released while it runs returns `Continue`; its
/// continuation is dropped on the worker, and a panic in a translator's
/// destructor there aborts the process (docs/threads.md 1.4). Before the
/// fix, the drop came after the job's guard: the panic ended the worker
/// thread while `live` still counted it, so a later pool task never ran and
/// `finish` never returned. The scenario runs in a child process (this test
/// binary again), which must end with SIGABRT and the crate's message.
#[test]
#[cfg_attr(miri, ignore)]
fn rt1_01_a_panic_in_a_dropped_continuation_aborts() {
    if std::env::var_os(RT1_01_CHILD).is_some() {
        rt1_01_child();
        return;
    }
    let _s = serial();
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "sched::mt::tests::rt1_01_a_panic_in_a_dropped_continuation_aborts",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(RT1_01_CHILD, "1")
        .output()
        .expect("the child runs");
    use std::os::unix::process::ExitStatusExt;
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.signal(),
        Some(6),
        "the child should abort; status {:?}, stderr:\n{err}",
        out.status
    );
    assert!(
        err.contains("lean-runtime: a Rust panic in"),
        "the crate's message; stderr:\n{err}"
    );
}

/// The child's scenario: it aborts at the continuation's drop; if it does
/// not, it says what happened and fails.
fn rt1_01_child() {
    let sh = start_test(1);
    let p = promise_new().unwrap();
    let (started, go) = (Gate::default(), Gate::default());
    let (s2, g2) = (started.clone(), go.clone());
    let t = spawn(
        Box::new(move || {
            s2.open();
            g2.wait();
            let pd = PanicOnDrop;
            Outcome::Continue(
                p,
                Box::new(move || {
                    let _keep = &pd;
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    started.wait();
    release(t);
    go.open();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let later = Arc::new(AtomicBool::new(false));
    let l2 = later.clone();
    spawn(
        Box::new(move || {
            l2.store(true, Ordering::SeqCst);
            Outcome::Done
        }),
        0,
        true,
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let ran = later.load(Ordering::SeqCst);
    let live = live_workers(&sh);
    let sh2 = sh.clone();
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    std::thread::spawn(move || {
        super::task::finish(&sh2);
        d2.store(true, Ordering::SeqCst);
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    let finished = done.load(Ordering::SeqCst);
    eprintln!(
        "the process did not abort; later pool task ran = {ran}, live workers = {live}, finish returned = {finished}"
    );
    std::process::exit(3);
}

/// RT1-02 (a) (the review's `rt1_continue_after_finish_makes_a_worker`):
/// after `finish` there is no task manager. A pool bind dependent of a
/// promise resolved then runs at once on the resolver; its function
/// returns `Continue` with a task that has finished, so it runs again at
/// once. Before the fix, `add_dep` enqueued it, and a standard worker was
/// made after `finish`, never joined.
#[test]
fn rt1_02a_a_continue_after_finish_runs_at_once() {
    let _s = serial();
    let sh = start_test(2);
    let p = promise_new().unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let r2 = ran.clone();
    let _d = depend(
        p,
        Box::new(move || {
            Outcome::Continue(
                TaskId::FINISHED,
                Box::new(move || {
                    r2.store(true, Ordering::SeqCst);
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
        false,
    );
    finish();
    assert_eq!(live_workers(&sh), 0);
    assert!(resolve(p, || {}));
    let after = live_workers(&sh);
    let ran_at_once = ran.load(Ordering::SeqCst);
    // a stray worker (the bug) ends before the assertion reports
    until(|| live_workers(&sh) == 0);
    assert!(
        ran_at_once && after == 0,
        "after finish: the continuation ran at once = {ran_at_once}, live workers right after the resolve = {after}"
    );
}

/// Hunt HMT-01: a bind task whose function returned the task itself waits
/// for itself, so for good, as natively (`add_dep(t, t)` puts `t` in its
/// own list of dependents): its continuation never runs, and its state stays
/// `waiting`. Before the fix it was queued again, and its continuation ran
/// with no value to read. One worker: `x`, made once `t`'s function has
/// returned and `t` is no longer running, runs after whatever that run
/// queued, so `x`'s end shows whether `t` ran again (review RF15-A04: a
/// wait for `t` to leave `running` alone passed while `t` was still queued
/// before its first run, and `x`, queued then, could run before `t` was
/// queued again). `keep_alive`: an IO task, or a pure one (the variant of
/// RF15-A04).
fn a_self_bind_waits_for_good(keep_alive: bool) {
    let _s = serial();
    start_test(1);
    let p = promise_new().unwrap();
    let me: Slot<TaskId> = Slot::default();
    let reran = Arc::new(AtomicBool::new(false));
    let returned = Arc::new(AtomicBool::new(false));
    let (me2, reran2, returned2) = (me.clone(), reran.clone(), returned.clone());
    let t = depend(
        p,
        Box::new(move || {
            let own = *me2.get().expect("stored before p resolves");
            returned2.store(true, Ordering::SeqCst);
            Outcome::Continue(
                own,
                Box::new(move || {
                    reran2.store(true, Ordering::SeqCst);
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
        keep_alive,
    );
    me.set(t).unwrap();
    assert!(resolve(p, || {}));
    until(|| returned.load(Ordering::SeqCst) && state(t) != TaskState::Running);
    let x = spawn(Box::new(|| Outcome::Done), 0, true);
    wait(x);
    assert!(!reran.load(Ordering::SeqCst), "the continuation ran");
    assert_eq!(state(t), TaskState::Waiting);
    // the translator's last reference: an IO task stays, as `release`
    // keeps every IO task until it has run (marked unreferenced); a pure
    // one stays by `release`'s held rule, marked deleted: a dependent in
    // the table, itself, holds it
    release(t);
    assert_eq!(state(t), TaskState::Waiting);
    finish();
}

#[test]
fn hmt_01_a_bind_task_that_continues_as_itself_waits_for_good() {
    a_self_bind_waits_for_good(true);
}

#[test]
fn hmt_01_a_pure_bind_task_that_continues_as_itself_waits_for_good() {
    a_self_bind_waits_for_good(false);
}

/// Hunt HMT-03: without a task manager a bind task's function runs at once,
/// and so does its continuation when the task it continues as has
/// finished: after `finish` (through the table since review RF15-A02, as
/// `add_dep` runs it then, RT1-02), and with `LEAN_NUM_THREADS=0`, where
/// no task manager ever ran (`run_at_once`). Before the fix the
/// continuation was dropped, and the glue's slot left empty for an id that
/// answers finished.
#[test]
fn hmt_03_without_a_task_manager_a_bind_task_continues_at_once() {
    let _s = serial();
    for workers in [1, 0] {
        start_test(workers);
        finish();
        let l = log();
        let l2 = l.clone();
        let id = depend(
            TaskId::FINISHED,
            Box::new(move || {
                Outcome::Continue(
                    TaskId::FINISHED,
                    Box::new(move || {
                        push(&l2, "continued");
                        Outcome::Done
                    }),
                )
            }),
            0,
            false,
            true,
        );
        assert!(is_finished(id), "{workers} workers");
        assert_eq!(entries(&l), ["continued"], "{workers} workers");
    }
}

/// Review RF15-A02: after `finish` a task's job runs at once, and a bind
/// task's `Continue` to a task that has not finished (here a promise
/// unresolved then) waits for it, as RT1-02's `add_dep` makes it: the id
/// answers unfinished, and the continuation runs when the promise is
/// resolved, on the resolving thread, and reads its value. Before the fix
/// the continuation ran at once, before `spawn` or `depend` returned, and
/// read the promise's empty slot. Both calls, each with its own promise.
#[test]
fn rf15_a02_after_finish_a_continue_to_an_unfinished_task_waits() {
    let _s = serial();
    let sh = start_test(1);
    let promises = [promise_new().unwrap(), promise_new().unwrap()];
    finish();
    for (k, p) in promises.into_iter().enumerate() {
        let value: Slot<u32> = Slot::default();
        let seen: Slot<Option<u32>> = Slot::default();
        let (v2, s2) = (value.clone(), seen.clone());
        let job: Job = Box::new(move || {
            Outcome::Continue(
                p,
                Box::new(move || {
                    let _ = s2.set(v2.get().copied());
                    Outcome::Done
                }),
            )
        });
        let id = if k == 0 {
            spawn(job, 0, true)
        } else {
            depend(TaskId::FINISHED, job, 0, false, true)
        };
        assert_eq!(seen.get(), None, "{k}: the continuation ran at once");
        assert!(!is_finished(id), "{k}");
        assert_eq!(state(id), TaskState::Waiting, "{k}");
        let v3 = value.clone();
        assert!(resolve(p, move || {
            let _ = v3.set(7);
        }));
        assert_eq!(seen.get(), Some(&Some(7)), "{k}");
        assert!(is_finished(id), "{k}");
    }
    assert_eq!(table_len(&sh), 0);
    assert_eq!(live_workers(&sh), 0);
}

/// Hunt HMT-04: a pure bind task released while it runs, which a dependent
/// still holds (`release`: it runs on as a started one), continues as an
/// unreleased one when its function returns `Continue`, so its dependent
/// gets its value. Before the fix its entry was removed and its
/// continuation dropped, and the dependent waited for good. (No translator
/// releases a task that a dependent's job still holds; the crate's rule
/// covers it.)
#[test]
fn hmt_04_a_held_released_bind_task_continues() {
    let _s = serial();
    start_test(2);
    let p = promise_new().unwrap();
    let out: Slot<u32> = Slot::default();
    let (started, go) = (Gate::default(), Gate::default());
    let (s2, g2, o2) = (started.clone(), go.clone(), out.clone());
    let b = spawn(
        Box::new(move || {
            s2.open();
            g2.wait();
            Outcome::Continue(
                p,
                Box::new(move || {
                    let _ = o2.set(42);
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    let seen: Slot<u32> = Slot::default();
    let o3 = out.clone();
    let d = depend(
        b,
        filling(&seen, move || o3.get().copied().unwrap_or(0)),
        0,
        false,
        true,
    );
    started.wait();
    release(b);
    go.open();
    until(|| state(b) != TaskState::Running);
    assert!(resolve(p, || {}));
    let t0 = std::time::Instant::now();
    while !is_finished(d) && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(is_finished(d), "the dependent waits for good");
    assert_eq!(seen.get(), Some(&42));
    finish();
}

/// Counts the threads the manager makes (`thread_start`).
struct CountingGlue(Arc<AtomicUsize>);

impl Glue for CountingGlue {
    fn thread_start(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// RT1-02 (b) (the review's
/// `rt1_wait_in_dependent_run_after_finish_makes_a_thread`): a pool
/// dependent of a promise resolved after `finish` runs at once on the
/// resolver, on that thread, so it holds no worker: a `wait` in it raises
/// no limit and makes no thread. Before the fix its frame said `pool`, and
/// the wait made a worker after `finish`.
#[test]
fn rt1_02b_a_wait_in_a_dependent_run_after_finish_makes_no_thread() {
    let _s = serial();
    let made = Arc::new(AtomicUsize::new(0));
    let sh = bind_local();
    configure(&sh, Arc::new(CountingGlue(made.clone())), 1, 256 << 10);
    let p = promise_new().unwrap();
    let q = promise_new().unwrap();
    let _d = depend(
        p,
        Box::new(move || {
            wait(q);
            Outcome::Done
        }),
        0,
        false,
        false,
    );
    finish();
    let before = made.load(Ordering::SeqCst);
    // `q` is resolved later by a helper thread; `p` now, here (its
    // dependent runs at once on this thread and waits for `q` meanwhile)
    let sh2 = sh.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(super::task::resolve(&sh2, q, || {}));
    });
    assert!(resolve(p, || {}));
    h.join().unwrap();
    until(|| live_workers(&sh) == 0);
    let after = made.load(Ordering::SeqCst);
    assert_eq!(
        after,
        before,
        "threads made by the manager after finish: {}",
        after - before
    );
}

/// Review AR-32: `running_worker` is the standard worker's index on its
/// thread; `None` on a dedicated task's thread and on `main`'s (a `sync`
/// dependent run there by `resolve` included).
#[test]
fn running_worker_names_the_pool_worker_thread() {
    let _s = serial();
    start_test(1);
    assert_eq!(running_worker(), None, "main");
    type Seen = Arc<StdMutex<Vec<(&'static str, Option<u32>)>>>;
    let seen: Seen = Arc::default();
    let rec = |tag: &'static str| -> Job {
        let s = seen.clone();
        Box::new(move || {
            s.lock().unwrap().push((tag, running_worker()));
            Outcome::Done
        })
    };
    let a = spawn(rec("a"), 0, true);
    wait(a);
    let b = spawn(rec("b"), 0, false);
    wait(b);
    let d = spawn(rec("dedicated"), 9, true);
    wait(d);
    let p = promise_new().unwrap();
    let dep = depend(p, rec("sync on main"), 0, true, true);
    assert!(resolve(p, || {}));
    assert!(is_finished(dep));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            ("a", Some(0)),
            ("b", Some(0)),
            ("dedicated", None),
            ("sync on main", None)
        ]
    );
    assert_eq!(running_worker(), None, "main again");
    finish();
}

/// Review AR-34: `finish` joins the standard workers, then calls
/// `Glue::workers_end` once, before it waits for the dedicated threads: a
/// dedicated task that waits for that hook goes on.
#[test]
fn the_workers_end_once_before_the_dedicated_threads_are_waited_for() {
    struct EndGlue {
        ends: AtomicU32,
        gate: Gate,
    }
    impl Glue for EndGlue {
        fn workers_end(&self) {
            self.ends.fetch_add(1, Ordering::SeqCst);
            self.gate.open();
        }
    }
    let _s = serial();
    let gate = Gate::default();
    let glue = Arc::new(EndGlue {
        ends: AtomicU32::new(0),
        gate: Gate(gate.0.clone()),
    });
    let sh = bind_local();
    configure(&sh, glue.clone(), 1, 256 << 10);
    let pool = spawn(Box::new(|| Outcome::Done), 0, true);
    wait(pool);
    // the dedicated task waits (at most 10 s) for `workers_end`
    let saw = Arc::new(AtomicBool::new(false));
    let saw2 = saw.clone();
    let g2 = Gate(gate.0.clone());
    spawn(
        Box::new(move || {
            let (m, cv) = &*g2.0;
            let r = cv
                .wait_timeout_while(m.lock().unwrap(), std::time::Duration::from_secs(10), |o| {
                    !*o
                })
                .unwrap();
            saw2.store(*r.0, Ordering::SeqCst);
            Outcome::Done
        }),
        9,
        true,
    );
    finish();
    assert!(
        saw.load(Ordering::SeqCst),
        "workers_end came before the dedicated thread ended"
    );
    assert_eq!(glue.ends.load(Ordering::SeqCst), 1);
}

/// Natively a worker's take of its next queued task (the loop's `dequeue`,
/// `m_idle_std_workers--`) comes in the hold of its last task's
/// `resolve_core` (hunt HMT2-01). `c` and `d` are queued while both
/// workers are busy (the limit is 2); `a` finishes, and its worker takes
/// `c` before its `task_end` hook, which the glue holds open. Then `b`, a
/// pool task, waits for `d`: the raise finds no idle worker and makes one,
/// which runs `d` while `c` waits for it. Before the fix the worker in its
/// hook counted idle with `c` still queued: the raise made no worker, and
/// `d` ran only after `c`.
#[test]
fn hmt2_01_a_worker_takes_its_next_task_before_its_task_end() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(true, std::time::Duration::from_secs(60)));
    configure(&sh, glue.clone(), 2, 256 << 10);
    let (a_started, b_started) = (Gate::default(), Gate::default());
    let (go_a, go_b, d_ran, d_go) = (
        Gate::default(),
        Gate::default(),
        Gate::default(),
        Gate::default(),
    );
    let d_id: Slot<TaskId> = Slot::default();
    let (b_started2, go_b2, d_id2) = (b_started.clone(), go_b.clone(), d_id.clone());
    let tb = spawn(
        Box::new(move || {
            b_started2.open();
            go_b2.wait();
            wait(*d_id2.get().unwrap());
            Outcome::Done
        }),
        0,
        true,
    );
    let (a_started2, go_a2) = (a_started.clone(), go_a.clone());
    let ta = spawn(
        Box::new(move || {
            a_started2.open();
            go_a2.wait();
            Outcome::Done
        }),
        0,
        true,
    );
    a_started.wait();
    b_started.wait();
    // both workers are busy: `c` and `d` stay queued
    let saw_d: Slot<bool> = Slot::default();
    let d_ran2 = d_ran.clone();
    let tc = spawn(
        filling(&saw_d, move || {
            d_ran2.wait_at_most(std::time::Duration::from_millis(500))
        }),
        0,
        true,
    );
    let (d_ran3, d_go2) = (d_ran.clone(), d_go.clone());
    let td = spawn(
        Box::new(move || {
            d_ran3.open();
            d_go2.wait_at_most(std::time::Duration::from_secs(60));
            Outcome::Done
        }),
        0,
        true,
    );
    d_id.set(td).unwrap();
    go_a.open();
    // `a` has finished; its worker took `c` and is in its `task_end`
    glue.in_hook.wait();
    assert!(is_finished(ta));
    go_b.open();
    // `b` waits for `d` (the limit is 3), until `d_go` opens
    until(|| blocked_waits(&sh) == 1);
    let live = live_workers(&sh);
    d_go.open();
    glue.go.open();
    wait(tc);
    wait(td);
    wait(tb);
    finish();
    assert_eq!(
        live, 3,
        "b's raise found a's worker busy with c: a new worker"
    );
    assert_eq!(saw_d.get(), Some(&true), "d ran while c waited for it");
}

/// The same with `c` and `d` queued while `a`'s worker is in its
/// `task_end` hook, counted idle (fixes-19): natively the worker waits on
/// `m_queue_cv` by then, and `c`'s enqueue wakes it to take `c`. Here it
/// cannot wait on `queue_cv` in its hook, so `c` is handed to it under the
/// lock (`wake_one`); it counts busy from then on and runs `c` once its
/// hook returns. So `b`'s raise makes a worker for `d`. Before the fix the
/// raise found the worker idle and woke nobody, and `d` ran only after `c`.
#[test]
fn hmt2_01_a_task_queued_during_a_workers_task_end_is_handed_to_it() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(true, std::time::Duration::from_secs(60)));
    configure(&sh, glue.clone(), 2, 256 << 10);
    let (go_b, d_ran, d_go) = (Gate::default(), Gate::default(), Gate::default());
    let d_id: Slot<TaskId> = Slot::default();
    let (go_b2, d_id2) = (go_b.clone(), d_id.clone());
    let tb = spawn(
        Box::new(move || {
            go_b2.wait();
            wait(*d_id2.get().unwrap());
            Outcome::Done
        }),
        0,
        true,
    );
    let a: Slot<Option<u32>> = Slot::default();
    let ta = spawn(filling(&a, running_worker), 0, true);
    // `a` has finished; its worker is in its `task_end`
    glue.in_hook.wait();
    assert!(is_finished(ta));
    let c: Slot<(Option<u32>, bool)> = Slot::default();
    let d_ran2 = d_ran.clone();
    let tc = spawn(
        filling(&c, move || {
            let w = running_worker();
            (
                w,
                d_ran2.wait_at_most(std::time::Duration::from_millis(500)),
            )
        }),
        0,
        true,
    );
    let (d_ran3, d_go2) = (d_ran.clone(), d_go.clone());
    let td = spawn(
        Box::new(move || {
            d_ran3.open();
            d_go2.wait_at_most(std::time::Duration::from_secs(60));
            Outcome::Done
        }),
        0,
        true,
    );
    d_id.set(td).unwrap();
    go_b.open();
    // `b` waits for `d` (the limit is 3), until `d_go` opens
    until(|| blocked_waits(&sh) == 1);
    let live = live_workers(&sh);
    d_go.open();
    glue.go.open();
    wait(tc);
    wait(td);
    wait(tb);
    finish();
    assert_eq!(
        live, 3,
        "b's raise found a's worker busy with c: a new worker"
    );
    let (c_worker, saw_d) = *c.get().unwrap();
    assert_eq!(c_worker, *a.get().unwrap(), "c ran on a's worker");
    assert!(saw_d, "d ran while c waited for it");
}

/// `end_running_task` walks the task's dependents inside its job, which
/// then finishes up; the worker counts itself idle only at the job's end,
/// at `run_one`'s `task_end` (hunt HMT2-03). Its waiters wake after that,
/// as after a walk `run_one` makes (fixes-19): `main`, blocked in `wait(a)`
/// before the job ends its task, goes on only once `a`'s worker is idle, so
/// `b` goes to it. Before the fix the walk woke `main` at once, and `b`,
/// queued while the job finished up (here until `main` has queued it, at
/// most 300 ms), made a second worker.
#[test]
fn hmt2_03_end_running_task_wakes_its_waiters_once_the_worker_is_idle() {
    let _s = serial();
    let sh = start_test(4);
    let id: Slot<TaskId> = Slot::default();
    let a: Slot<Option<u32>> = Slot::default();
    let main_queued = Gate::default();
    let (id2, a2, sh2, mq) = (id.clone(), a.clone(), sh.clone(), main_queued.clone());
    let ta = spawn(
        Box::new(move || {
            until(|| blocked_waits(&sh2) == 1);
            let _ = a2.set(running_worker());
            end_running_task(*id2.get().unwrap());
            mq.wait_at_most(std::time::Duration::from_millis(300));
            Outcome::Done
        }),
        0,
        true,
    );
    id.set(ta).unwrap();
    wait(ta);
    let b: Slot<Option<u32>> = Slot::default();
    let tb = spawn(filling(&b, running_worker), 0, true);
    let live = live_workers(&sh);
    main_queued.open();
    wait(tb);
    finish();
    assert_eq!(live, 1, "b's enqueue found a's worker idle: no new worker");
    assert_eq!(b.get(), a.get(), "b ran on a's worker");
}

/// The glue's rule for a full slot in threads mode (hunt HMT2-02;
/// docs/sched.md, "The glue", item 3): the job stores the value outside
/// the lock, and the task leaves the table only in the next hold, which
/// also walks its dependents and counts its worker idle. Natively one hold
/// sets `m_value` and goes on to the worker's next task. Here the job holds
/// that gap open after its store: the slot is full, but
/// `full_slot_finished` answers false, and `state` running, as native's
/// answer while `m_value` is still null. Once the hold has ended (the
/// worker is in its `task_end` hook, which the glue holds open), it answers
/// true, and a task queued then goes to that worker.
#[test]
fn hmt2_02_a_full_slot_is_finished_only_once_its_workers_hold_ended() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(true, std::time::Duration::from_secs(60)));
    configure(&sh, glue.clone(), 4, 256 << 10);
    let a: Slot<Option<u32>> = Slot::default();
    let (stored, go) = (Gate::default(), Gate::default());
    let (a2, stored2, go2) = (a.clone(), stored.clone(), go.clone());
    let ta = spawn(
        Box::new(move || {
            let _ = a2.set(running_worker());
            stored2.open();
            go2.wait_at_most(std::time::Duration::from_secs(60));
            Outcome::Done
        }),
        0,
        true,
    );
    stored.wait();
    assert!(a.get().is_some(), "the job has stored its value");
    assert!(!full_slot_finished(ta), "a full slot, not finished yet");
    assert_eq!(state(ta), TaskState::Running);
    go.open();
    glue.in_hook.wait();
    assert!(full_slot_finished(ta), "finished once the hold ended");
    let b: Slot<Option<u32>> = Slot::default();
    let tb = spawn(filling(&b, running_worker), 0, true);
    let live = live_workers(&sh);
    glue.go.open();
    wait(tb);
    finish();
    assert_eq!(live, 1, "b's enqueue found a's worker idle: no new worker");
    assert_eq!(b.get(), a.get(), "b ran on a's worker");
}

/// The hunter's repro with the glue's rule (native program `hmt2_02.lean`:
/// `main` spins on `IO.hasFinished a`, then queues `b`; one worker thread
/// in every run): `main` takes `a` as finished once its slot is full and
/// `full_slot_finished` agrees, so the hold that counts `a`'s worker idle
/// has ended, and `b` goes to that worker in every round. Read with the
/// slot alone (the rule before HMT2-02), `b` often found the worker busy
/// and made a new one.
#[test]
fn hmt2_02_a_task_seen_finished_then_a_new_task_takes_its_worker() {
    let _s = serial();
    let sh = start_test(4);
    for round in 0..20 {
        let a: Slot<Option<u32>> = Slot::default();
        let ta = spawn(filling(&a, running_worker), 0, true);
        while !(a.get().is_some() && full_slot_finished(ta)) {
            std::hint::spin_loop();
        }
        let b: Slot<Option<u32>> = Slot::default();
        let tb = spawn(filling(&b, running_worker), 0, true);
        wait(ta);
        wait(tb);
        assert_eq!(b.get(), a.get(), "round {round}: b ran on a's worker");
    }
    let live = live_workers(&sh);
    finish();
    assert_eq!(live, 1, "b always went to a's worker, as natively");
}

// ---------------------------------------------------------------------------
// hunt-mt3 (third look at fixes-22)

/// HMT3-01 (a): a walk is one hold of the lock. Natively `resolve_core`
/// queues every pool dependent of the walk with `enqueue_core` while it
/// holds `m_mutex`: an idle worker woken by the first `notify_one` cannot
/// take a task before the walk ends, so every enqueue of the walk finds it
/// idle and no worker is made. Here the lone worker is in its `task_end`
/// hook (counted idle); `main` resolves a promise with two pool dependents.
/// Native: 1 worker (it runs `d2`, then `d1`).
#[test]
fn hmt3_01a_a_walk_that_queues_two_tasks_during_a_hook_makes_no_worker() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(true, std::time::Duration::from_secs(60)));
    configure(&sh, glue.clone(), 4, 256 << 10);
    let a: Slot<Option<u32>> = Slot::default();
    let ta = spawn(filling(&a, running_worker), 0, true);
    // `a` has finished; its worker is in its `task_end`, counted idle
    glue.in_hook.wait();
    assert!(is_finished(ta));
    let p = promise_new().unwrap();
    let (d1, d2): (Slot<Option<u32>>, Slot<Option<u32>>) = Default::default();
    let t1 = depend(p, filling(&d1, running_worker), 0, false, true);
    let t2 = depend(p, filling(&d2, running_worker), 0, false, true);
    // one hold: the walk queues `d2`, then `d1`
    assert!(resolve(p, || {}));
    let live = live_workers(&sh);
    glue.go.open();
    wait(t1);
    wait(t2);
    finish();
    assert_eq!(
        live, 1,
        "both enqueues of one walk find the idle worker: no new worker (native)"
    );
}

/// HMT3-01 (b): natively the worker that finishes `s` keeps `m_mutex`
/// from `resolve_core` through `m_idle_std_workers++` to the loop's
/// `dequeue`: the dependent its walk queued first (the newest, `d2`) goes
/// to that worker in every run, and an idle worker woken meanwhile finds
/// the queue without it. Here worker 1 is in its `task_end` hook (counted
/// idle) while worker 0 walks `s`'s two pool dependents. Native: `d2` runs
/// on `s`'s worker in every run (`d1` on either worker), and no third
/// worker is made (native program `hmt3_01b.lean`).
#[test]
fn hmt3_01b_a_walk_on_a_worker_keeps_its_first_dependent_while_another_is_in_its_hook() {
    let _s = serial();
    let sh = bind_local();
    let glue = Arc::new(HoldEndGlue::new(true, std::time::Duration::from_secs(60)));
    configure(&sh, glue.clone(), 4, 256 << 10);
    let (s_started, go_s) = (Gate::default(), Gate::default());
    let s_w: Slot<Option<u32>> = Slot::default();
    let (s_started2, go_s2, s_w2) = (s_started.clone(), go_s.clone(), s_w.clone());
    let ts = spawn(
        Box::new(move || {
            let _ = s_w2.set(running_worker());
            s_started2.open();
            go_s2.wait();
            Outcome::Done
        }),
        0,
        true,
    );
    s_started.wait();
    // `a` gets a second worker (the first is busy with `s`); its
    // `task_end` is held: that worker counts idle in its hook
    let a: Slot<Option<u32>> = Slot::default();
    let ta = spawn(filling(&a, running_worker), 0, true);
    glue.in_hook.wait();
    assert!(is_finished(ta));
    let (d1, d2): (Slot<Option<u32>>, Slot<Option<u32>>) = Default::default();
    let t1 = depend(ts, filling(&d1, running_worker), 0, false, true);
    let t2 = depend(ts, filling(&d2, running_worker), 0, false, true);
    go_s.open();
    // `s`'s walk and its worker's take step are over once `main` wakes
    wait(ts);
    let live = live_workers(&sh);
    glue.go.open();
    wait(t1);
    wait(t2);
    finish();
    assert_eq!(
        (live, *d2.get().unwrap()),
        (2, *s_w.get().unwrap()),
        "d2 ran on s's worker and no third worker was made (native); d1 ran on {:?}, a on {:?}",
        d1.get(),
        a.get()
    );
}

/// HMT3-02: the glues' drop rule (docs/sched.md, The glue, item 3).
/// Natively the last reference dropped after the closure returned and
/// before the worker's `lock.lock()` (`m_value` still null) deactivates
/// the task (`deactivate_task_core`: `m_deleted`), and its finish frees it
/// with no `resolve_core`, so no `notify_all`. The glues' rule before
/// fixes-24 ("`release` while the slot is empty; a full slot is a task
/// that is finishing") skipped `release` there (the slot is full), so the
/// finish notified: here it woke the waiter of `p`, whose walk is still in
/// progress (an older `sync` dependent holds it), as RS2-08's
/// `wait_any_unref_finish` shows. The test emulates the glue's drop with
/// today's rule: `release` unless the task is confirmed (ids are never
/// reused); it fails with the old rule.
#[test]
fn hmt3_02_a_task_dropped_after_its_store_notifies_nobody() {
    let _s = serial();
    let _sh = start_test(4);
    let p = promise_new().unwrap();
    let go_slow = Gate::default();
    let go_slow2 = go_slow.clone();
    let slow = depend(
        p,
        Box::new(move || {
            go_slow2.wait_at_most(std::time::Duration::from_secs(60));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    // a dedicated task waits for `p`
    let woke = Gate::default();
    let woke2 = woke.clone();
    let tw = spawn(
        Box::new(move || {
            wait(p);
            woke2.open();
            Outcome::Done
        }),
        9,
        true,
    );
    until(|| blocked_waits(&_sh) == 1);
    // another dedicated task resolves `p`: its walk runs `slow`, which
    // holds it; `p` is out of the table, its waiter not notified yet
    let tr = spawn(
        Box::new(move || {
            assert!(resolve(p, || {}));
            Outcome::Done
        }),
        9,
        true,
    );
    until(|| is_finished(p));
    // an IO task stores its value, and its last reference goes before the
    // worker's hold
    let x: Slot<u32> = Slot::default();
    let (stored, go_x) = (Gate::default(), Gate::default());
    let (x2, stored2, go_x2) = (x.clone(), stored.clone(), go_x.clone());
    let tx = spawn(
        Box::new(move || {
            let _ = x2.set(1);
            stored2.open();
            go_x2.wait_at_most(std::time::Duration::from_secs(60));
            Outcome::Done
        }),
        0,
        true,
    );
    stored.wait();
    assert!(x.get().is_some());
    // the handle's drop: no answer of the scheduler confirmed `tx` (the
    // glue's `confirmed` is false), so `release`, though its slot is full
    let confirmed = false;
    if !confirmed {
        release(tx);
    }
    go_x.open();
    until(|| is_finished(tx));
    let early = woke.wait_at_most(std::time::Duration::from_millis(300));
    go_slow.open();
    wait(slow);
    wait(tr);
    wait(tw);
    finish();
    assert!(
        !early,
        "the finish of a task whose last reference went before its hold notified p's waiter"
    );
}
