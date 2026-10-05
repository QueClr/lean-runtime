//! Unit tests of threads mode, small sizes. Each test has a task manager of
//! its own (`bind_local`), bound to the test's thread and to the threads it
//! makes, so tests run in parallel; each ends with `finish`, which joins
//! every thread it made (Miri runs these tests and checks that no thread is
//! left). Gates (a lock and a condition variable of std) hold a task where
//! a test needs it, so the checks do not depend on timing; the few sleeps
//! only make the other order likely, never required.

use super::sync::{Condvar, Mutex, RecursiveMutex, SharedMutex};
use super::task::{bind_local, configure, live_workers, table_len, wake_waiters, Shared};
use super::*;
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
pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
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
    go.open();
    for id in [c0, b0, a8, m4] {
        wait(id);
    }
    assert_eq!(
        entries(&l),
        ["dedicated", "blocker", "a8", "m4", "c0", "b0"]
    );
    finish();
}

#[test]
fn sync_priority_runs_at_once_on_the_calling_thread() {
    let _s = serial();
    start_test(2);
    let me = thread_number();
    let seen: Slot<(u64, bool)> = Slot::default();
    let id = spawn(
        filling(&seen, || (thread_number(), in_sync_task())),
        u64::from(u32::MAX),
        false,
    );
    // it ran inside `spawn`, as a task on this thread
    assert!(is_finished(id));
    assert_eq!(seen.get(), Some(&(me, true)));
    assert!(!in_sync_task());
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
    let l = log();
    let l2 = l.clone();
    let s = spawn(
        Box::new(move || {
            Outcome::Continue(
                TaskId::FINISHED,
                Box::new(move || {
                    l2.lock().unwrap().push("continued".into());
                    Outcome::Done
                }),
            )
        }),
        u64::from(u32::MAX),
        false,
    );
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
        // the other thread's operation now waits for this store
        std::thread::sleep(std::time::Duration::from_millis(5));
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
