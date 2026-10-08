//! Unit tests of the scheduler's bookkeeping. Each test runs on a thread of
//! its own, so with a scheduler of its own. No context ever suspends here:
//! the crate cannot hold the glue's `unsafe` suspend step, so these tests
//! use tasks that do not block on a context of their own; the programs that
//! block are in `tests/sched-driver`. Tests that create a context switch
//! stacks, which Miri cannot run.

use super::sync::{Mutex, RecursiveMutex, SharedMutex};
use super::*;
use std::cell::{Cell, RefCell};

struct NoSuspend;

impl Glue for NoSuspend {
    fn suspend(&self, _: Suspend<'_>) {
        panic!("the crate's unit tests never suspend a context");
    }
}

fn start_test(workers: u32) {
    start_with(Rc::new(NoSuspend), workers, 1 << 20);
}

type Log = Rc<RefCell<Vec<String>>>;

fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

fn job(l: &Log, s: &str) -> Job {
    let l = l.clone();
    let s = s.to_string();
    Box::new(move || {
        l.borrow_mut().push(s);
        Outcome::Done
    })
}

fn entries(l: &Log) -> Vec<String> {
    l.borrow().clone()
}

/// Whether the host stalled during a sleep of `ms` begun at `t0`: it took a
/// tenth longer than asked, or 2 ms, whichever is more. The hub wakes a
/// sleeper whose deadline has passed before it starts a queued task, so
/// after a stall as long as the sleep, a task meant to run while `main`
/// slept may not have started yet, as natively a worker slower than the
/// sleep would not have; the sleep then takes at least the stall, while
/// without one it ends right at its deadline. A test then skips its check of
/// what ran during the sleep, with a note, and checks only what holds
/// whatever the timing (the review of the flaky `sched::` tests, sched-2;
/// checked with injected stalls of 6 and 60 ms).
fn stalled(t0: std::time::Instant, ms: u32) -> bool {
    let took = t0.elapsed();
    let slack = (u64::from(ms) / 10).max(2);
    let late = took >= std::time::Duration::from_millis(u64::from(ms) + slack);
    if late {
        eprintln!(
            "note: the host stalled ({took:?} for a {ms} ms sleep): a check of what ran during the sleep is skipped"
        );
    }
    late
}

/// `sleep_ms(ms)`, then whether the host stalled meanwhile (`stalled`).
fn sleep_or_stall(ms: u32) -> bool {
    let t0 = std::time::Instant::now();
    sleep_ms(ms);
    stalled(t0, ms)
}

/// A translator's task reference: the last one releases the task.
struct Handle(TaskId);

impl Drop for Handle {
    fn drop(&mut self) {
        release(self.0);
    }
}

struct DropFlag(Rc<Cell<bool>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
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
}

#[test]
fn no_task_manager_with_zero_workers() {
    // LEAN_NUM_THREADS=0: `lean_init_task_manager_using(0)` creates none.
    start_test(0);
    assert!(!manager_running());
    let l = log();
    assert_eq!(spawn(job(&l, "a"), 0, false), TaskId::FINISHED);
    assert_eq!(entries(&l), ["a"]);
    assert!(promise_new().is_err());
}

fn start_lazy_test(workers: u32) {
    start_lazy(Rc::new(NoSuspend), workers, 1 << 20);
}

static LAZY_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A lazy-start test: one at a time, and `ST.Ref` read yields (process-wide),
/// which the lazy start turns on, off again at its end, so that no other
/// unit test sees them on (review RSH2-01).
struct LazyTest(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Drop for LazyTest {
    fn drop(&mut self) {
        set_ref_read_yields(false);
    }
}

fn lazy_test() -> LazyTest {
    LazyTest(
        LAZY_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// The lazy start (audit item 4.5): the numbers are taken at once, the
/// scheduler is built at the first task. Until then `deferring` already
/// says the task manager runs, while `manager_running` (built with
/// workers) does not; afterwards both, and the task is deferred as with
/// `start`.
#[test]
fn the_lazy_start_waits_for_the_first_task() {
    let _t = lazy_test();
    assert!(!sched_started() && !deferring());
    ensure_started();
    assert!(!sched_started(), "nothing to start before start_lazy");
    start_lazy_test(4);
    assert!(!sched_started());
    assert!(deferring());
    assert!(!manager_running());
    let l = log();
    let a = spawn(job(&l, "a"), 0, true);
    assert!(sched_started() && manager_running() && deferring());
    // `ST.Ref` reads are polling points from the start on (process-wide;
    // no unit test turns them off)
    assert!(REF_YIELDS.load(std::sync::atomic::Ordering::Relaxed));
    assert!(entries(&l).is_empty(), "deferred, as after start");
    assert!(!is_finished(a));
    wait(a);
    assert_eq!(entries(&l), ["a"]);
    finish();
}

/// A promise, a dependent and every `Std.Sync` constructor start it too.
#[test]
fn the_lazy_start_comes_with_a_promise() {
    let _t = lazy_test();
    start_lazy_test(2);
    assert!(promise_new().is_ok());
    assert!(sched_started() && manager_running());
}

#[test]
fn the_lazy_start_comes_with_a_dependent() {
    let _t = lazy_test();
    start_lazy_test(2);
    // `Task.pure x |>.map f`: the source is no task, and the dependent is a
    // task (natively deferred), so it starts the scheduler first
    assert!(!dependent_runs_now(TaskId::FINISHED, false));
    assert!(sched_started());
}

#[test]
fn the_lazy_start_comes_with_std_sync() {
    let _t = lazy_test();
    let made: [fn(); 4] = [
        || drop(Mutex::new()),
        || drop(sync::Condvar::new()),
        || drop(RecursiveMutex::new()),
        || drop(SharedMutex::new()),
    ];
    for (k, make) in made.into_iter().enumerate() {
        // each on a thread of its own: the state is the thread's
        std::thread::spawn(move || {
            start_lazy_test(1);
            make();
            assert!(sched_started(), "constructor {k}");
        })
        .join()
        .unwrap();
    }
}

/// lean2rr's review RS4-05: a recursive mutex made by an initializer
/// (before `start_lazy`), locked by `main` before its first task and again
/// after it, has one owner, so the nested lock does not wait: `main`'s OS
/// thread and context are the same before the lazy start and after it
/// (AR-39; before AR-39 the owner held whether the scheduler had started,
/// and the first lock in `main` had to start it).
#[test]
fn std_sync_in_main_starts_it_first() {
    let _t = lazy_test();
    let m = RecursiveMutex::new();
    assert!(!sched_started(), "an initializer starts nothing");
    start_lazy_test(2);
    m.lock();
    assert!(sched_started());
    let l = log();
    let a = spawn(job(&l, "a"), 0, true);
    m.lock();
    m.unlock();
    m.unlock();
    wait(a);
    assert_eq!(entries(&l), ["a"]);
    finish();
}

/// How `main` starts the scheduler in the owner tests (AR-39): lazily or at
/// once, with workers or with none (`LEAN_NUM_THREADS=0`).
const STARTS: [(bool, u32); 4] = [(true, 2), (true, 0), (false, 2), (false, 0)];

fn start_main(lazy: bool, workers: u32) {
    if lazy {
        start_lazy_test(workers);
    } else {
        start_test(workers);
    }
}

/// AR-39 (lean2rr's review RS7-02, two of its four shapes): a lock's owner
/// is the OS thread, not whether the scheduler has started. An initializer
/// keeps a recursive mutex locked, and `main` runs on the same OS thread
/// (`LEAN_MAIN_USE_THREAD=0`): it locks the mutex again, with workers and
/// with none, after a lazy start and after an eager one, as natively. Before
/// AR-39, with workers, `main`'s `try_lock` failed and its `lock` waited for
/// good.
#[test]
fn a_recursive_lock_is_the_os_threads_across_the_start() {
    let _t = lazy_test();
    for (lazy, workers) in STARTS {
        // each on a thread of its own: the scheduler's state is the thread's
        std::thread::spawn(move || {
            // the initializers
            let m = RecursiveMutex::new();
            m.lock();
            assert!(!sched_started());
            // `main`, on the initializers' thread
            start_main(lazy, workers);
            assert!(m.try_lock(), "lazy {lazy}, workers {workers}");
            assert!(sched_started());
            m.lock();
            for _ in 0..3 {
                m.unlock();
            }
            // free again: a dedicated task (another thread) takes it
            let took = Rc::new(Cell::new(false));
            let t = spawn(
                Box::new({
                    let (m, took) = (Rc::new(m), took.clone());
                    move || {
                        took.set(m.try_lock());
                        Outcome::Done
                    }
                }),
                9,
                true,
            );
            wait(t);
            assert!(took.get(), "lazy {lazy}, workers {workers}");
            finish();
        })
        .join()
        .unwrap();
    }
}

/// AR-39, the other two shapes: an initializer on the process's first
/// thread keeps a recursive mutex locked, and `main` runs on a thread of its
/// own (`run_main`): the mutex is another thread's, with workers and with
/// none, as natively. Before AR-39, with no workers, `main` took it (both
/// owners were `main`'s context of a scheduler with no task manager).
/// `relocking_from_another_os_thread_hangs` shows `main`'s `lock` waiting.
#[test]
fn a_recursive_lock_from_another_os_thread_waits() {
    let _t = lazy_test();
    for (lazy, workers) in STARTS {
        // the initializers, on a thread that then ends (the object is
        // `Send`: plain data)
        let m = std::thread::spawn(|| {
            let m = RecursiveMutex::new();
            m.lock();
            m
        })
        .join()
        .unwrap();
        std::thread::spawn(move || {
            start_main(lazy, workers);
            assert!(!m.try_lock(), "lazy {lazy}, workers {workers}");
            assert!(sched_started());
        })
        .join()
        .unwrap();
    }
}

/// AR-39: `main` on a thread of its own with no workers
/// (`LEAN_NUM_THREADS=0`) locks a recursive mutex that an initializer, on
/// another OS thread, keeps locked: it waits forever, as natively (a
/// deadlock). With no other context the hub has nothing to run, and the
/// thread sleeps for good (`hang_thread`): no Rust panic, and nothing
/// printed. Before AR-39 it took the lock. In a child process, which the
/// test kills.
#[test]
#[cfg_attr(miri, ignore)]
fn relocking_from_another_os_thread_hangs() {
    const CHILD: &str = "LEAN_RUNTIME_TEST_RELOCK_HANGS";
    if std::env::var_os(CHILD).is_some() {
        let m = std::thread::spawn(|| {
            let m = RecursiveMutex::new();
            m.lock();
            m
        })
        .join()
        .unwrap();
        std::thread::spawn(move || {
            start_lazy_test(0);
            println!("main locks");
            m.lock();
            println!("main took the lock");
        })
        .join()
        .unwrap();
        return;
    }
    use std::io::{BufRead, Read};
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sched::tests::relocking_from_another_os_thread_hangs",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut seen = String::new();
    while !seen.contains("main locks\n") {
        let n = out.read_line(&mut seen).unwrap();
        assert!(n > 0, "the child ended before its lock; stdout {seen:?}");
    }
    // a lock that does not wait returns at once: the child would print and
    // end well within this
    std::thread::sleep(std::time::Duration::from_millis(300));
    let still = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    out.read_to_string(&mut seen).unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(still, "the child ended: stdout {seen:?}, stderr {err:?}");
    assert!(!seen.contains("main took the lock"), "stdout {seen:?}");
    assert!(!err.contains("panicked"), "stderr {err:?}");
}

/// With no workers (`LEAN_NUM_THREADS=0`) nothing is deferred: tasks run
/// at once, after the scheduler is built (its glue for the task's job).
#[test]
fn the_lazy_start_with_no_workers() {
    let _t = lazy_test();
    start_lazy_test(0);
    assert!(!deferring());
    let l = log();
    assert_eq!(spawn(job(&l, "a"), 0, false), TaskId::FINISHED);
    assert_eq!(entries(&l), ["a"]);
    assert!(sched_started() && !manager_running() && !deferring());
    assert!(promise_new().is_err());
}

/// `finish` after a lazy start that never came builds nothing, and the
/// scheduler stays unstarted; `start_with` after `start_lazy` starts at
/// once and drops what `start_lazy` took; `start_lazy` after the start is
/// `start_with`.
#[test]
fn the_lazy_start_with_finish_and_start() {
    let _t = lazy_test();
    std::thread::spawn(|| {
        start_lazy_test(3);
        finish();
        assert!(!sched_started());
    })
    .join()
    .unwrap();
    std::thread::spawn(|| {
        start_lazy_test(3);
        start_test(0);
        assert!(sched_started() && !deferring());
        ensure_started();
        assert!(!manager_running(), "start_with's numbers, not start_lazy's");
        start_lazy_test(2);
        assert!(sched_started() && deferring() && manager_running());
    })
    .join()
    .unwrap();
}

#[test]
fn tasks_are_deferred_until_needed() {
    start_test(4);
    let l = log();
    let a = spawn(job(&l, "a"), 0, true);
    let b = spawn(job(&l, "b"), 0, false);
    assert!(entries(&l).is_empty());
    assert!(!is_finished(a));
    // `b` is not the queue's head: the waiter waits while a worker takes
    // `a`, then `b`, first come, first served (AR-10).
    wait(b);
    assert_eq!(entries(&l), ["a", "b"]);
    wait(a);
    assert!(is_finished(a) && is_finished(b));
    // the head runs on the waiter's stack, as a free worker would run it
    let c = spawn(job(&l, "c"), 0, true);
    wait(c);
    assert_eq!(entries(&l), ["a", "b", "c"]);
    finish();
    assert_eq!(entries(&l), ["a", "b", "c"]);
}

#[test]
fn the_final_run_goes_by_priority() {
    start_test(1);
    let l = log();
    // Created inside a task (`in_task`), so that no idle worker wakes for
    // them: Lean's queues alone decide the order.
    let l2 = l.clone();
    in_task(move || {
        spawn(job(&l2, "default 1"), 0, true);
        spawn(job(&l2, "max"), 8, true);
        spawn(job(&l2, "prio 3"), 3, true);
        spawn(job(&l2, "default 2"), 0, true);
        spawn(job(&l2, "dedicated"), 9, true);
        // Every priority above 8 is dedicated, whatever its low 32 bits
        // (LB-39): native's `unsigned` makes 2^32 - 1 `LEAN_SYNC_PRIO` and
        // 2^32 + 4 priority 4; `u64::MAX` is a big `Nat`, saturated.
        spawn(job(&l2, "prio 2^32-1"), u64::from(u32::MAX), true);
        spawn(job(&l2, "prio 2^32+4"), (1 << 32) + 4, true);
        spawn(job(&l2, "big prio"), u64::MAX, true);
    });
    assert!(entries(&l).is_empty());
    finish();
    assert_eq!(
        entries(&l),
        [
            "dedicated",
            "prio 2^32-1",
            "prio 2^32+4",
            "big prio",
            "max",
            "prio 3",
            "default 1",
            "default 2"
        ]
    );
}

/// No priority runs a spawn at once on the spawning thread (LB-39): above
/// 8, `LEAN_SYNC_PRIO`'s 2^32 - 1 included, it is a dedicated task, which
/// runs as on a thread of its own, on no pool worker, and is no `sync`
/// task.
#[test]
fn a_big_priority_is_dedicated_never_sync() {
    start_test(4);
    for prio in [9, u64::from(u32::MAX), 1 << 32, (1 << 32) + 8, u64::MAX] {
        let seen = Rc::new(Cell::new(None));
        let seen2 = seen.clone();
        let id = spawn(
            Box::new(move || {
                seen2.set(Some((thread_number(), in_sync_task(), running_worker())));
                Outcome::Done
            }),
            prio,
            true,
        );
        assert!(!is_finished(id), "priority {prio} ran inside spawn");
        assert_eq!(seen.get(), None);
        wait(id);
        let (th, sync, worker) = seen.get().expect("it ran");
        assert_ne!(th, 0, "priority {prio} ran on main's thread");
        assert!(!sync, "priority {prio} ran as a sync task");
        assert_eq!(worker, None, "priority {prio} ran on a pool worker");
    }
}

/// `sync` alone makes a dependent run at once on the thread that finishes
/// its source (here the resolving one, `main`'s), as a `sync` task; its
/// priority plays no part. A dependent at 2^32 - 1 without `sync` is
/// queued as a dedicated task (LB-39).
#[test]
fn a_sync_dependent_runs_at_once_whatever_its_priority() {
    start_test(4);
    for prio in [0, 8, u64::from(u32::MAX), u64::MAX] {
        let p = promise_new().unwrap();
        let seen = Rc::new(Cell::new(None));
        let seen2 = seen.clone();
        let d = depend(
            p,
            Box::new(move || {
                seen2.set(Some((thread_number(), in_sync_task())));
                Outcome::Done
            }),
            prio,
            true,
            true,
        );
        assert!(!is_finished(d));
        assert!(resolve(p, || {}));
        assert!(is_finished(d), "the sync dependent ran inside resolve");
        assert_eq!(seen.get(), Some((0, true)), "priority {prio}");
    }
    let p = promise_new().unwrap();
    let seen = Rc::new(Cell::new(None));
    let seen2 = seen.clone();
    let d = depend(
        p,
        Box::new(move || {
            seen2.set(Some((thread_number(), in_sync_task(), running_worker())));
            Outcome::Done
        }),
        u64::from(u32::MAX),
        false,
        true,
    );
    assert!(resolve(p, || {}));
    assert!(!is_finished(d), "an async dependent is queued");
    wait(d);
    let (th, sync, worker) = seen.get().expect("it ran");
    assert_ne!(th, 0);
    assert!(!sync);
    assert_eq!(worker, None, "a dedicated task is on no pool worker");
}

#[test]
fn a_needed_task_runs_as_on_a_worker_thread() {
    start_test(4);
    let th = Rc::new(Cell::new(u64::MAX));
    let th2 = th.clone();
    let id = spawn(
        Box::new(move || {
            th2.set(thread_number());
            Outcome::Done
        }),
        0,
        true,
    );
    assert_eq!(thread_number(), 0);
    wait(id);
    assert_eq!(th.get(), 1);
}

#[test]
fn dependents_are_walked_newest_first_and_sync_ones_run_there() {
    // tasks/sync_dependent_order, with a promise as the source.
    start_test(1);
    let l = log();
    let p = promise_new().unwrap();
    depend(p, job(&l, "async dep"), 0, false, true);
    let l2 = l.clone();
    depend(
        p,
        Box::new(move || {
            l2.borrow_mut().push("sync dep".into());
            spawn(job(&l2, "made by sync dep"), 0, true);
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    depend(p, job(&l, "newest async dep"), 0, false, true);
    let l3 = l.clone();
    assert!(resolve(p, move || l3.borrow_mut().push("resolved".into())));
    assert_eq!(entries(&l), ["resolved", "sync dep"]);
    // Only the first resolution counts.
    assert!(!resolve(p, || panic!("resolved twice")));
    finish();
    assert_eq!(
        entries(&l),
        [
            "resolved",
            "sync dep",
            "newest async dep",
            "made by sync dep",
            "async dep"
        ]
    );
}

#[test]
fn a_sync_dependent_of_a_finished_task_is_not_a_task() {
    start_test(4);
    let l = log();
    let a = spawn(job(&l, "a"), 0, true);
    assert!(!dependent_runs_now(a, true));
    wait(a);
    assert!(dependent_runs_now(a, true));
    assert!(!dependent_runs_now(a, false));
    // Not sync: a task, queued at once.
    let d = depend(a, job(&l, "d"), 0, false, true);
    assert!(!is_finished(d));
    wait(d);
    assert_eq!(entries(&l), ["a", "d"]);
}

#[test]
fn a_long_chain_runs_from_its_deepest_end() {
    start_test(4);
    let l = log();
    let first = spawn(job(&l, "0"), 0, false);
    let mut last = first;
    let n = if cfg!(miri) { 200 } else { 20_000 };
    for k in 1..n {
        last = depend(last, job(&l, &k.to_string()), 0, false, false);
    }
    wait(last);
    let e = entries(&l);
    assert_eq!(e.len(), n);
    assert_eq!(e[0], "0");
    assert_eq!(e[n - 1], (n - 1).to_string());
}

#[test]
fn a_dropped_pure_task_never_runs() {
    start_test(4);
    let l = log();
    let flag = Rc::new(Cell::new(false));
    let d = DropFlag(flag.clone());
    let l2 = l.clone();
    let id = spawn(
        Box::new(move || {
            let _ = &d;
            l2.borrow_mut().push("ran".into());
            Outcome::Done
        }),
        0,
        false,
    );
    release(id);
    assert!(
        flag.get(),
        "its computation is freed at once, as Lean's deactivate_task"
    );
    assert!(is_finished(id));
    finish();
    assert!(entries(&l).is_empty());
}

/// Run `f` as a task on `main`'s thread: a `sync` dependent of a promise
/// that `main` resolves, which runs inside `resolve`, as a `sync` task, on
/// `main`'s thread as natively. The scheduler wakes no idle worker for an
/// enqueue by a running task, so the tasks `f` creates wait in their
/// queues: the helper keeps the lone worker's pick (`settle_worker`, by
/// elapsed time) out of the test.
fn in_task(f: impl FnOnce() + 'static) {
    let p = promise_new().unwrap();
    let d = depend(
        p,
        Box::new(move || {
            f();
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    assert!(resolve(p, || {}));
    assert!(is_finished(d), "the sync dependent ran inside resolve");
}

/// `await_task` (`Task.get` once the glue's slot is empty): in a `sync`
/// task it reports `GET_IN_SYNC_TASK`, then waits; elsewhere it only waits;
/// for `TaskId::FINISHED` it does neither.
#[test]
fn await_task_reports_in_a_sync_task_then_waits() {
    start_test(4);
    let l = log();
    let reports: Rc<RefCell<Vec<String>>> = Rc::default();
    // pure tasks: deferred until awaited
    let pending = spawn(job(&l, "pending"), 0, false);
    let (r, l2) = (reports.clone(), l.clone());
    in_task(move || {
        await_task(TaskId::FINISHED, |m| r.borrow_mut().push(m.to_owned()));
        await_task(pending, |m| r.borrow_mut().push(m.to_owned()));
        l2.borrow_mut().push("sync".into());
    });
    assert_eq!(*reports.borrow(), [GET_IN_SYNC_TASK]);
    assert_eq!(entries(&l), ["pending", "sync"]);
    let other = spawn(job(&l, "other"), 0, false);
    await_task(other, |m| panic!("no report outside a sync task: {m}"));
    assert!(is_finished(other));
    assert_eq!(entries(&l), ["pending", "sync", "other"]);
}

/// `IO.getTID` (`io::env::get_tid`): `gettid` plus the number of the thread
/// a task natively runs on (`tid_offset`): `main`'s id in a `sync`
/// dependent that `main` runs (`in_task`), and another one in a task a
/// worker runs (the first worker's: 1).
#[cfg(feature = "io")]
#[test]
fn get_tid_tells_the_tasks_threads_apart() {
    start_test(4);
    let main = crate::io::env::get_tid();
    assert_eq!(main, nix::unistd::gettid().as_raw() as u64);
    let (a, b) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let a2 = a.clone();
    in_task(move || a2.set(crate::io::env::get_tid()));
    let b2 = b.clone();
    let id = spawn(
        Box::new(move || {
            b2.set(crate::io::env::get_tid());
            Outcome::Done
        }),
        0,
        true,
    );
    wait(id);
    assert_eq!(a.get(), main);
    assert_eq!(b.get(), main + 1);
}

#[test]
fn dropping_a_dependent_releases_its_source() {
    start_test(4);
    let l = log();
    let l2 = l.clone();
    in_task(move || {
        let src = Handle(spawn(job(&l2, "source"), 0, false));
        let sid = src.0;
        let dep = Handle(depend(
            sid,
            {
                let l = l2.clone();
                Box::new(move || {
                    let _ = &src;
                    l.borrow_mut().push("dependent".into());
                    Outcome::Done
                })
            },
            0,
            false,
            false,
        ));
        assert!(!is_finished(sid));
        drop(dep);
        assert!(
            is_finished(sid),
            "the source was deleted with the dependent that held it"
        );
    });
    finish();
    assert!(entries(&l).is_empty());
}

#[test]
fn a_dropped_io_task_still_runs() {
    start_test(4);
    let l = log();
    let id = spawn(job(&l, "io"), 0, true);
    release(id);
    assert!(!is_finished(id));
    finish();
    assert_eq!(entries(&l), ["io"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_started_pure_task_runs_although_dropped() {
    // tasks/runaway_pure_task_started, with a task that ends.
    start_test(1);
    let l = log();
    let id = spawn(job(&l, "pure"), 0, false);
    assert_eq!(state(id), TaskState::Waiting);
    // A worker starts it during the sleep: natively it runs in parallel;
    // here it is only marked started.
    sleep_ms(1);
    assert!(entries(&l).is_empty());
    assert_eq!(
        state(id),
        TaskState::Running,
        "one sleep: still running for the poller"
    );
    release(id);
    finish();
    assert_eq!(entries(&l), ["pure"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_started_pure_task_runs_once_an_io_task_waits_for_it() {
    start_test(1);
    let l = log();
    let p = spawn(job(&l, "pure"), 0, false);
    sleep_ms(1);
    assert!(entries(&l).is_empty(), "started, not run");
    depend(p, job(&l, "io dependent"), 0, false, true);
    // While `main` sleeps, the pure task runs on a context, and its IO
    // dependent after it, as natively they would have by then. (A worker
    // context lets a context that can go on run first: the sleep must not
    // end before both have run.)
    if !sleep_or_stall(50) {
        assert_eq!(entries(&l), ["pure", "io dependent"]);
    }
    finish();
    assert_eq!(entries(&l), ["pure", "io dependent"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_io_task_waiting_through_pure_tasks_starts_them() {
    // Review RS1S-01 of sched-1: `t := Task.spawn f; u := t.map g;
    // v := u.map h; IO.mapTask k v`. The IO task's need reaches `t` through
    // the pure tasks, so while `main` sleeps all four run, as natively.
    start_test(1);
    let l = log();
    let l2 = l.clone();
    in_task(move || {
        let t = spawn(job(&l2, "t"), 0, false);
        let u = depend(t, job(&l2, "u"), 0, false, false);
        let v = depend(u, job(&l2, "v"), 0, false, false);
        depend(v, job(&l2, "io"), 0, false, true);
    });
    if !sleep_or_stall(50) {
        assert_eq!(entries(&l), ["t", "u", "v", "io"]);
    }
    finish();
    assert_eq!(entries(&l), ["t", "u", "v", "io"]);
}

#[test]
fn io_need_reaches_up_a_chain_of_pure_tasks() {
    // The IO task gives `u` IO need, and `u` gives it to `t`; waiting for
    // the IO task runs the chain from its deepest end.
    start_test(4);
    let l = log();
    let l2 = l.clone();
    in_task(move || {
        let t = spawn(job(&l2, "t"), 0, false);
        let u = depend(t, job(&l2, "u"), 0, false, false);
        let io = depend(u, job(&l2, "io"), 0, false, true);
        with(|s| {
            let (ti, ui) = (s.find(t).unwrap(), s.find(u).unwrap());
            assert_eq!((s.need_of(ti), s.need_of(ui)), (1, 1));
        });
        wait(io);
        assert!(is_finished(t) && is_finished(u));
    });
    assert_eq!(entries(&l), ["t", "u", "io"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn polling_with_sleeps_runs_a_pure_task_after_two() {
    start_test(1);
    let l = log();
    let id = spawn(job(&l, "pure"), 0, false);
    let mut answers = Vec::new();
    loop {
        let st = state(id);
        answers.push(st);
        if st == TaskState::Finished {
            break;
        }
        sleep_ms(1);
    }
    assert_eq!(
        answers,
        [TaskState::Waiting, TaskState::Running, TaskState::Finished]
    );
    assert_eq!(entries(&l), ["pure"]);
}

#[test]
fn polling_in_a_busy_loop_runs_the_task() {
    start_test(4);
    let l = log();
    let id = spawn(job(&l, "io"), 0, true);
    let mut n = 0;
    while state(id) != TaskState::Finished {
        n += 1;
    }
    assert_eq!(n, 1000);
    assert_eq!(entries(&l), ["io"]);
}

#[test]
fn cancellation_reaches_dependents() {
    start_test(4);
    let l = log();
    assert!(!check_canceled(), "false in main");
    let p = promise_new().unwrap();
    let l2 = l.clone();
    let d = depend(
        p,
        Box::new(move || {
            l2.borrow_mut()
                .push(format!("canceled {}", check_canceled()));
            Outcome::Done
        }),
        0,
        false,
        true,
    );
    cancel(p);
    resolve(p, || {});
    wait(d);
    // Created after it finished: not canceled.
    let l3 = l.clone();
    let a = spawn(
        Box::new(move || {
            l3.borrow_mut().push(format!("fresh {}", check_canceled()));
            Outcome::Done
        }),
        0,
        true,
    );
    wait(a);
    assert_eq!(entries(&l), ["canceled true", "fresh false"]);
}

#[test]
fn check_canceled_at_shutdown() {
    // tasks/checkcanceled_after_main: a task queued when `main` returned
    // sees the shutdown flag from its second check on, or after a sleep;
    // one it creates afterwards could only start after the flag was set, and
    // sees it at once.
    start_test(4);
    let l = log();
    let l2 = l.clone();
    spawn(
        Box::new(move || {
            let first = check_canceled();
            let second = check_canceled();
            l2.borrow_mut().push(format!("queued {first} {second}"));
            let l3 = l2.clone();
            spawn(
                Box::new(move || {
                    l3.borrow_mut()
                        .push(format!("created at shutdown {}", check_canceled()));
                    Outcome::Done
                }),
                0,
                true,
            );
            Outcome::Done
        }),
        0,
        true,
    );
    finish();
    assert_eq!(
        entries(&l),
        ["queued false true", "created at shutdown true"]
    );
}

#[test]
fn a_bind_task_continues_as_the_task_it_returned() {
    start_test(4);
    let l = log();
    let t2 = spawn(job(&l, "inner"), 0, true);
    let l2 = l.clone();
    let b = spawn(
        Box::new(move || {
            l2.borrow_mut().push("f".into());
            let l3 = l2.clone();
            Outcome::Continue(
                t2,
                Box::new(move || {
                    l3.borrow_mut().push("copy inner's value".into());
                    Outcome::Done
                }),
            )
        }),
        0,
        true,
    );
    // `t2` is the queue's head, so it runs first (AR-10), then `b`, which
    // continues as `t2`, already finished
    wait(b);
    assert_eq!(entries(&l), ["inner", "f", "copy inner's value"]);
}

#[test]
fn wait_any_takes_a_finished_task_else_waits_for_the_heads() {
    start_test(4);
    let l = log();
    let p = promise_new().unwrap();
    let a = spawn(job(&l, "a"), 0, true);
    let b = spawn(job(&l, "b"), 0, true);
    assert_eq!(wait_any(&[p, a, b]), 1);
    assert_eq!(entries(&l), ["a"]);
    wait(b);
    assert_eq!(wait_any(&[p, a, b]), 1);
    resolve(p, || {});
    assert_eq!(wait_any(&[p, a, b]), 0);
}

#[test]
fn promise_states() {
    start_test(4);
    let p = promise_new().unwrap();
    assert_eq!(
        state(p),
        TaskState::Running,
        "an unresolved promise is running, as natively"
    );
    assert!(!is_finished(p));
    resolve(p, || {});
    assert_eq!(state(p), TaskState::Finished);
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_io_task_starts_on_a_context_while_main_sleeps() {
    start_test(2);
    let l = log();
    let id = spawn(job(&l, "io"), 0, true);
    if !sleep_or_stall(50) {
        assert_eq!(entries(&l), ["io"], "the IO task ran while main slept");
        assert!(is_finished(id));
    }
    finish();
    assert_eq!(entries(&l), ["io"]);
    assert!(is_finished(id));
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_effect_point_lets_a_stale_task_go_first() {
    start_test(2);
    let l = log();
    spawn(job(&l, "queued 6 ms ago"), 0, true);
    let fresh = spawn(job(&l, "fresh"), 0, true);
    std::thread::sleep(std::time::Duration::from_millis(6));
    let fresh2 = spawn(job(&l, "queued now"), 0, true);
    effect();
    // Both were queued over 5 ms ago (`STALE`) and run first; the one queued
    // just now waits, unless the host stalled for 5 ms meanwhile.
    assert_eq!(entries(&l)[..2], ["queued 6 ms ago", "fresh"]);
    assert!(is_finished(fresh));
    finish();
    assert!(is_finished(fresh2));
    assert_eq!(entries(&l), ["queued 6 ms ago", "fresh", "queued now"]);
}

/// A glue whose `switched` hook blocks, which the hub forbids (it runs on
/// `main`'s stack; docs/sched.md, S4).
struct BlockingSwitch;

impl Glue for BlockingSwitch {
    fn suspend(&self, _: Suspend<'_>) {
        panic!("the crate's unit tests never suspend a context");
    }
    fn switched(&self, _: CtxId, _: CtxId) {
        block_sync();
    }
}

/// The hook's attempt to block is refused with a panic, which aborts the
/// process (review RS1S-12): the test runs itself again as a child process.
#[test]
#[cfg_attr(miri, ignore)]
fn a_hub_hook_cannot_block() {
    const CHILD: &str = "LEAN_RUNTIME_TEST_BLOCKING_HOOK";
    if std::env::var_os(CHILD).is_some() {
        start_with(Rc::new(BlockingSwitch), 1, 1 << 20);
        // `main` waits for a promise no one resolves: the hub starts the
        // queued task on a context of its own, and `switched`, run before it
        // resumes that context, tries to block.
        let _t = spawn(Box::new(|| Outcome::Done), 9, true);
        let p = promise_new().unwrap();
        wait(p);
        unreachable!();
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sched::tests::a_hub_hook_cannot_block",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(6), "SIGABRT; stderr {err:?}");
    assert!(err.contains("must not block or yield"), "stderr {err:?}");
    assert!(err.contains("aborting"), "stderr {err:?}");
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_caught_context_panic_leaves_main_usable() {
    // From the reviewer of sched-1 (RS1S-04): after a context's panic goes
    // on in `main` and is caught there, `main` blocks and runs tasks again.
    start_test(2);
    spawn(Box::new(|| panic!("boom in a context")), 0, true);
    let t0 = std::time::Instant::now();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sleep_ms(50)));
    if r.is_ok() {
        // `main` woke before the context started: only a stall explains it,
        // and the panicking task is still queued: nothing more to check.
        assert!(stalled(t0, 50), "the context's panic goes on in main");
        return;
    }
    assert_eq!(current_context(), MAIN);
    with(|s| {
        assert_eq!(s.cx.ctxs[MAIN].status, ctx::Status::Running);
        assert_eq!(s.cx.blocked, 0);
        assert_eq!(s.cx.workers, 0);
    });
    let l = log();
    let after = spawn(job(&l, "after"), 0, true);
    if sleep_or_stall(50) && entries(&l).is_empty() {
        wait(after);
    }
    assert_eq!(entries(&l), ["after"]);
}

#[test]
fn locks_without_contention() {
    start_test(4);
    let m = Mutex::new();
    assert!(m.try_lock());
    assert!(!m.try_lock(), "glibc's mutex is not recursive");
    m.unlock();
    m.lock();
    m.unlock();
    let r = RecursiveMutex::new();
    r.lock();
    assert!(r.try_lock());
    r.unlock();
    r.unlock();
    assert!(r.try_lock());
    r.unlock();
    let s = SharedMutex::new();
    s.read();
    assert!(s.try_read());
    assert!(!s.try_write());
    s.unlock_read();
    s.unlock_read();
    assert!(s.try_write());
    assert!(!s.try_read());
    s.unlock_write();
}

// --- From the reviewer of sched-1, round 2: the IO-need invariant, and a
// panic caught on `main` (RS1S-12).

fn need_ok() {
    with(|s| s.check_need());
}

#[test]
#[cfg_attr(miri, ignore)]
fn review2_need_cancel_release_midchain() {
    start_test(4);
    let l = log();
    let l2 = l.clone();
    in_task(move || {
        let t = spawn(job(&l2, "t"), 0, false);
        let u = depend(t, job(&l2, "u"), 0, false, false);
        let u2 = depend(t, job(&l2, "u2"), 0, false, false);
        need_ok();
        let io = depend(u, job(&l2, "io"), 0, false, true);
        need_ok();
        cancel(u);
        cancel(t);
        need_ok();
        release(u2);
        need_ok();
        // A pure sibling with need of its own, then its source finishes.
        let v = depend(t, job(&l2, "v"), 0, false, false);
        let io2 = depend(v, job(&l2, "io2"), 0, false, true);
        need_ok();
        wait(t); // t finishes mid-chain: u and v are queued with need
        need_ok();
        wait(io);
        need_ok();
        wait(io2);
        need_ok();
    });
    need_ok();
    finish();
    need_ok();
    assert_eq!(entries(&l).len(), 5);
}

#[test]
#[cfg_attr(miri, ignore)]
fn review2_need_through_bind_to_a_picked_task() {
    start_test(1);
    let l = log();
    let w = spawn(job(&l, "w"), 0, false);
    sleep_ms(1); // a worker picks w, which has no IO need
    assert_eq!(state(w), TaskState::Running, "picked");
    need_ok();
    let l2 = l.clone();
    let l3 = l.clone();
    in_task(move || {
        let t = spawn(job(&l2, "t"), 0, false);
        let l4 = l2.clone();
        let b = depend(
            t,
            Box::new(move || {
                l4.borrow_mut().push("b".into());
                let l5 = l4.clone();
                Outcome::Continue(
                    w,
                    Box::new(move || {
                        l5.borrow_mut().push("b2".into());
                        Outcome::Done
                    }),
                )
            }),
            0,
            false,
            false,
        );
        depend(b, job(&l3, "io"), 0, false, true);
        need_ok();
    });
    let late = sleep_or_stall(50);
    need_ok();
    if !late {
        assert_eq!(entries(&l), ["t", "b", "w", "b2", "io"]);
    }
    finish();
    need_ok();
    // After a stall the final run takes the started `w` first: only the set
    // of tasks that ran is the same.
    let mut e = entries(&l);
    e.sort();
    assert_eq!(e, ["b", "b2", "io", "t", "w"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn review2_need_cycle_counts() {
    // d (a pure bind) comes to wait for s, a pure map of d; an IO task waits
    // for d.
    start_test(4);
    let l = log();
    let sid: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let sid2 = sid.clone();
    let l2 = l.clone();
    let x0 = spawn(job(&l, "x0"), 0, false);
    let d = depend(
        x0,
        Box::new(move || {
            l2.borrow_mut().push("d".into());
            let l3 = l2.clone();
            Outcome::Continue(
                sid2.get().unwrap(),
                Box::new(move || {
                    l3.borrow_mut().push("d2".into());
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
        false,
    );
    let s = depend(d, job(&l, "s"), 0, false, false);
    sid.set(Some(s));
    let io = depend(d, job(&l, "io"), 0, false, true);
    need_ok();
    // x0 and d run on a worker context; d now waits for s (after a stall,
    // the final run runs them instead).
    let late = sleep_or_stall(50);
    need_ok();
    if !late {
        assert_eq!(entries(&l), ["x0", "d"]);
    }
    // d waits for s and s for d: neither ever finishes, so neither does
    // the IO task, as natively (a `wait(io)` waits forever; review HL2-03:
    // before, `wait` ran s, waiting in no queue, before d had finished).
    // The final run leaves them.
    assert!(!is_finished(d) && !is_finished(s) && !is_finished(io));
    finish();
    need_ok();
    assert_eq!(entries(&l), ["x0", "d"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn review2_caught_panic_through_an_inline_task() {
    // `main` runs t1 inline (`wait`), and t1 sleeps; meanwhile p panics on a
    // context; the panic unwinds through t1's run on `main` and is caught
    // there.
    start_test(2);
    let t1 = spawn(
        Box::new(|| {
            sleep_ms(50);
            Outcome::Done
        }),
        0,
        true,
    );
    spawn(Box::new(|| panic!("boom in a context")), 0, true);
    let t0 = std::time::Instant::now();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait(t1)));
    if r.is_ok() {
        // t1's sleep ended before the context started: only a stall explains
        // it, and the panicking task is still queued: nothing more to check.
        assert!(stalled(t0, 50), "the panic unwinds through t1's run");
        return;
    }
    assert_eq!(thread_number(), 0, "main's bookkeeping still holds t1");
    assert_eq!(with(|s| s.cx.in_use), 0, "a worker still counted in use");
    assert!(!is_finished(t1), "the abandoned task stays unfinished");
    need_ok();
    // `main` goes on.
    let l = log();
    let after = spawn(job(&l, "after"), 0, true);
    if sleep_or_stall(50) && entries(&l).is_empty() {
        wait(after);
    }
    assert_eq!(entries(&l), ["after"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_caught_panic_in_a_sync_dependent_ends_its_walk() {
    // `main` runs t inline; the walk of t's dependents runs the newest, a
    // `sync` one, which panics. The walk ends there: the other dependent is
    // queued, and runs later (RS1S-12).
    start_test(2);
    let l = log();
    let t = spawn(job(&l, "t"), 0, true);
    let other = depend(t, job(&l, "other"), 0, true, true);
    let bad = depend(
        t,
        Box::new(|| panic!("boom in a sync dependent")),
        0,
        true,
        true,
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait(t)));
    assert!(r.is_err());
    assert!(is_finished(t));
    assert!(!is_finished(bad), "the abandoned task stays unfinished");
    assert_eq!(state(other), TaskState::Waiting, "queued");
    assert_eq!(thread_number(), 0);
    assert_eq!(with(|s| s.cx.in_use), 0);
    need_ok();
    wait(other);
    assert_eq!(entries(&l), ["t", "other"]);
}

// ---------------------------------------------------------------------------
// The event loop (sched-io). Its callbacks run on the loop context, a
// context of its own that never suspends here.

/// A promise that `main` waits for, a callback resolving it, and the count
/// of its calls.
type Waitable = (TaskId, Rc<dyn Fn()>, Rc<Cell<u32>>);

fn waitable() -> Waitable {
    let p = promise_new().unwrap();
    let n = Rc::new(Cell::new(0));
    let n2 = n.clone();
    let cb: Rc<dyn Fn()> = Rc::new(move || {
        n2.set(n2.get() + 1);
        resolve(p, || {});
    });
    (p, cb, n)
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_timer_runs_on_the_loop_context_when_due() {
    start_test(2);
    let (p, cb, n) = waitable();
    let t0 = std::time::Instant::now();
    timer_start(t0 + std::time::Duration::from_millis(30), cb);
    assert!(
        io_cooperative(),
        "a pending timer: blocking calls cooperate"
    );
    // `main` blocks: the hub waits until the timer is due, then the loop
    // context runs its callback, which resolves the promise.
    wait(p);
    assert!(t0.elapsed() >= std::time::Duration::from_millis(30));
    assert_eq!(n.get(), 1);
    assert!(
        !io_cooperative(),
        "nothing left: blocking calls are plain again"
    );
    finish();
}

#[test]
#[cfg_attr(miri, ignore)]
fn timers_run_in_deadline_order_and_a_stopped_one_never_runs() {
    start_test(2);
    let l = log();
    let t0 = std::time::Instant::now();
    let at = |ms| t0 + std::time::Duration::from_millis(ms);
    let push = |s: &'static str| -> Rc<dyn Fn()> {
        let l = l.clone();
        Rc::new(move || l.borrow_mut().push(s.to_string()))
    };
    timer_start(at(20), push("20 first"));
    let stopped = timer_start(at(10), push("stopped"));
    timer_start(at(5), push("5"));
    timer_start(at(20), push("20 second"));
    let (p, cb, _) = waitable();
    timer_start(at(40), cb);
    assert!(timer_stop(stopped));
    assert!(!timer_stop(stopped));
    wait(p);
    assert_eq!(entries(&l), ["5", "20 first", "20 second"]);
    finish();
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_watch_runs_when_its_descriptor_is_readable() {
    start_test(2);
    let (r, w) = rustix::pipe::pipe().unwrap();
    let r = Rc::new(r);
    let p = promise_new().unwrap();
    let seen = Rc::new(Cell::new(Ready::default()));
    let (s2, r2) = (seen.clone(), r.clone());
    let id = watch(
        r.clone(),
        Interest::READ,
        Rc::new(move |ready| {
            s2.set(ready);
            // drained: once the call returns and the watch is armed again,
            // the loop finds nothing to queue (a second call, queued while
            // the byte was still there, would keep `io_cooperative` true
            // after `unwatch` until the loop skips it)
            rustix::io::read(&*r2, &mut [0u8; 1]).unwrap();
            resolve(p, || {});
        }),
    )
    .unwrap();
    // one watch per descriptor
    assert!(watch(r.clone(), Interest::READ, Rc::new(|_| {})).is_err());
    rustix::io::write(&w, b"x").unwrap();
    wait(p);
    assert!(seen.get().read);
    unwatch(id);
    assert!(!io_cooperative());
    // the reactor let go of its clone (and the refused watch's)
    assert_eq!(Rc::strong_count(&r), 1);
    finish();
}

/// The watch's id, set once `watch` has returned, for its own callback.
type IdCell = Rc<Cell<Option<WatchId>>>;

/// A watch may end itself from its callback: it is not armed again, and the
/// reactor lets go of the descriptor at once (net-1's guarantees 1 and 2).
#[test]
#[cfg_attr(miri, ignore)]
fn a_watch_unwatched_in_its_callback_lets_go_at_once() {
    start_test(2);
    let (r, w) = rustix::pipe::pipe().unwrap();
    let r = Rc::new(r);
    let weak = Rc::downgrade(&r);
    let p = promise_new().unwrap();
    let calls = Rc::new(Cell::new(0));
    let after_unwatch = Rc::new(Cell::new(usize::MAX));
    let id: IdCell = Rc::default();
    let (c2, a2, id2) = (calls.clone(), after_unwatch.clone(), id.clone());
    let wid = watch(
        r.clone(),
        Interest::READ,
        Rc::new(move |_| {
            c2.set(c2.get() + 1);
            unwatch(id2.get().unwrap());
            // only the test's own reference is left
            a2.set(weak.strong_count());
            resolve(p, || {});
        }),
    )
    .unwrap();
    id.set(Some(wid));
    rustix::io::write(&w, b"x").unwrap();
    wait(p);
    assert_eq!(calls.get(), 1);
    assert_eq!(after_unwatch.get(), 1);
    // still readable, but no longer watched: nothing is registered, and a
    // sleep (with a timer to make the loop look) calls nothing
    assert!(!io_cooperative());
    let (q, cb, _) = waitable();
    timer_start(
        std::time::Instant::now() + std::time::Duration::from_millis(20),
        cb,
    );
    wait(q);
    assert_eq!(calls.get(), 1);
    // the program drops its descriptor too: it is the last owner, so the
    // descriptor closes at once (`OwnedFd`'s drop)
    let weak = Rc::downgrade(&r);
    drop(r);
    assert!(weak.upgrade().is_none());
    drop(w);
    finish();
}

/// A watch modified from its callback is armed again with the new interest
/// (net-1's guarantee 1); a hang-up counts as ready for a read (guarantee
/// 3).
#[test]
#[cfg_attr(miri, ignore)]
fn a_watch_modified_in_its_callback_is_armed_with_the_new_interest() {
    start_test(2);
    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
    let a = Rc::new(a);
    let p = promise_new().unwrap();
    let seen: Rc<RefCell<Vec<Ready>>> = Rc::default();
    let id: IdCell = Rc::default();
    let (s2, id2) = (seen.clone(), id.clone());
    let wid = watch(
        a.clone(),
        Interest::READ,
        Rc::new(move |ready| {
            s2.borrow_mut().push(ready);
            let id = id2.get().unwrap();
            if s2.borrow().len() == 1 {
                // not read: still readable, but now watched for writing
                watch_modify(id, Interest::WRITE).unwrap();
            } else {
                unwatch(id);
                resolve(p, || {});
            }
        }),
    )
    .unwrap();
    id.set(Some(wid));
    use std::io::Write;
    (&b).write_all(b"x").unwrap();
    wait(p);
    let seen = seen.borrow();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].read);
    assert!(seen[1].write);
    drop(seen);
    // an end of file: POLLIN|POLLHUP
    let (r, w) = rustix::pipe::pipe().unwrap();
    let p = promise_new().unwrap();
    let got = Rc::new(Cell::new(Ready::default()));
    let g2 = got.clone();
    let wid = watch(
        r,
        Interest::READ,
        Rc::new(move |ready| {
            g2.set(ready);
            resolve(p, || {});
        }),
    )
    .unwrap();
    drop(w);
    wait(p);
    unwatch(wid);
    assert!(got.get().read && got.get().hangup);
    finish();
}

#[test]
fn poll_fds_without_tasks_is_poll() {
    use rustix::fd::AsFd;
    let (r, w) = rustix::pipe::pipe().unwrap();
    let mut it = [
        PollItem::new(r.as_fd(), Interest::READ),
        PollItem::new(w.as_fd(), Interest::WRITE),
    ];
    assert!(!io_cooperative());
    assert_eq!(
        poll_fds(&mut it, Some(std::time::Duration::ZERO)).unwrap(),
        1
    );
    assert!(!it[0].ready.read && it[1].ready.write);
    rustix::io::write(&w, b"x").unwrap();
    assert!(wait_fd(r.as_fd(), Interest::READ).unwrap().read);
}

/// Review AR-24: natively a task's standard streams (`IO.setStdout` & co.)
/// and `errno` are its thread's: a pool worker keeps them from one task to
/// the next; a new worker, a dedicated task's thread and `main` start with
/// the process's streams and `errno` 0. Here (one emulated worker,
/// `LEAN_NUM_THREADS=1`) a pool task's redirection and `errno` reach its
/// `sync` dependent (its thread's walk) and the next pool task; a dedicated
/// task starts fresh; `main` keeps its own. Before the fix every context
/// and task shared the thread's one set: `a` saw `main`'s, `main` saw `a`'s.
#[cfg(feature = "io")]
#[test]
#[cfg_attr(miri, ignore)]
fn a_pool_worker_keeps_its_streams_and_errno() {
    use crate::io::error::{errno, set_errno};
    use crate::io::streams::{self, StdStream};
    type Seen = Rc<Cell<Option<(u32, i32)>>>;
    fn out() -> u32 {
        streams::current(StdStream::Stdout, || 0u32)
    }
    fn record(seen: &Seen) -> Job {
        let seen = seen.clone();
        Box::new(move || {
            seen.set(Some((out(), errno())));
            Outcome::Done
        })
    }
    start_test(1);
    assert_eq!(streams::set_stdout(7u32, || 0), 0);
    set_errno(9);
    let a: Seen = Rc::default();
    let a2 = a.clone();
    let ta = spawn(
        Box::new(move || {
            a2.set(Some((out(), errno())));
            let _ = streams::set_stdout(5u32, || 0);
            set_errno(2);
            Outcome::Done
        }),
        0,
        true,
    );
    let d: Seen = Rc::default();
    let td = depend(ta, record(&d), 0, true, true);
    wait(td);
    let b: Seen = Rc::default();
    let tb = spawn(record(&b), 0, true);
    wait(tb);
    let c: Seen = Rc::default();
    let tc = spawn(record(&c), 9, true);
    wait(tc);
    finish();
    assert_eq!(a.get(), Some((0, 0)), "a new worker starts fresh");
    assert_eq!(d.get(), Some((5, 2)), "a sync dependent shares the thread");
    assert_eq!(b.get(), Some((5, 2)), "the worker kept them");
    assert_eq!(
        c.get(),
        Some((0, 0)),
        "a dedicated task's thread starts fresh"
    );
    assert_eq!((out(), errno()), (7, 9), "main keeps its own");
    assert_eq!(streams::set_stdout(0u32, || 0), 7);
}

thread_local! {
    /// A glue's own per-task state (lean2rr's stream context), for the tests
    /// of `end_running_task`.
    static GLUE_CTX: Cell<&'static str> = const { Cell::new("main's") };
}

/// The glue's protocol of review AR-26 (lean2rr's AR-S1) for a job whose
/// task id `id` holds once the scheduler runs it: open its own context
/// (`name`), store the value, `end_running_task(id)` (unless `end_first` is
/// false), close the context.
fn protocol_job(name: &'static str, id: Rc<Cell<TaskId>>, end_first: bool) -> Job {
    Box::new(move || {
        GLUE_CTX.with(|c| c.set(name));
        if end_first {
            end_running_task(id.get());
        }
        GLUE_CTX.with(|c| c.set("closed"));
        Outcome::Done
    })
}

/// A `sync` dependent of `src` that records the glue's context it runs in.
fn context_seen_by_sync_dependent(src: TaskId) -> (TaskId, Rc<Cell<Option<&'static str>>>) {
    let seen: Rc<Cell<Option<&'static str>>> = Rc::default();
    let s2 = seen.clone();
    let d = depend(
        src,
        Box::new(move || {
            s2.set(Some(GLUE_CTX.with(Cell::get)));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    (d, seen)
}

/// Review AR-26 (lean2rr's AR-S1): a job that opens its own context (a
/// translator's stream context, values only its code can drop), stores the
/// task's value, calls `end_running_task` and then closes the context: the
/// task's `sync` dependent runs inside the context, as natively it runs in
/// `handle_finished` with what the task left installed. A job that does not
/// call it (the control) has its dependent run after the context closed.
#[test]
fn a_job_ends_its_task_before_it_closes_its_context() {
    start_test(1);
    let ida: Rc<Cell<TaskId>> = Rc::new(Cell::new(TaskId::FINISHED));
    let a = spawn(protocol_job("the task's", ida.clone(), true), 0, true);
    ida.set(a);
    let (da, sa) = context_seen_by_sync_dependent(a);
    wait(da);
    let idb: Rc<Cell<TaskId>> = Rc::new(Cell::new(TaskId::FINISHED));
    let b = spawn(protocol_job("the task's", idb.clone(), false), 0, true);
    idb.set(b);
    let (db, sb) = context_seen_by_sync_dependent(b);
    wait(db);
    assert!(is_finished(a) && is_finished(b));
    finish();
    assert_eq!(sa.get(), Some("the task's"));
    assert_eq!(sb.get(), Some("closed"));
}

/// Review RT2-14: `end_running_task(id)` ends task `id` only while the
/// scheduler runs its job. A job that task A's job runs itself (the glue's
/// inline path: no task, so `TaskId::FINISHED`; or A's own id passed by
/// mistake from a second call) ends nothing: A's `sync` dependent still
/// sees the value A stores after it. Before the fix the call ended the
/// innermost running task, A, before A stored its value.
#[test]
fn rt2_14_end_running_task_ends_only_its_own_task() {
    start_test(1);
    let value: Rc<Cell<Option<u32>>> = Rc::default();
    let id: Rc<Cell<TaskId>> = Rc::new(Cell::new(TaskId::FINISHED));
    let (v2, id2) = (value.clone(), id.clone());
    let a = spawn(
        Box::new(move || {
            let inner: Job = Box::new(|| {
                end_running_task(TaskId::FINISHED);
                Outcome::Done
            });
            let _ = inner();
            v2.set(Some(42));
            end_running_task(id2.get());
            // a second call: nothing
            end_running_task(id2.get());
            Outcome::Done
        }),
        0,
        true,
    );
    id.set(a);
    let seen: Rc<Cell<Option<Option<u32>>>> = Rc::default();
    let (s2, v3) = (seen.clone(), value.clone());
    let d = depend(
        a,
        Box::new(move || {
            s2.set(Some(v3.get()));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    wait(d);
    finish();
    assert_eq!(
        seen.get(),
        Some(Some(42)),
        "A's dependent ran before A stored its value"
    );
}

/// Review RT2-15 (leanrs's AR26-01): in a chain A -> B -> C of `sync`
/// dependents, B (handed by A's walk) follows the protocol: C runs inside
/// B's context, as it does in threads mode and natively (`handle_finished`
/// runs C on the finishing thread). Before the fix `end` returned false for
/// a walked task, `end_running_task` walked nothing, and C ran after B's job
/// closed its context.
#[test]
fn rt2_15_end_running_task_in_a_walked_sync_dependent() {
    start_test(1);
    let ida: Rc<Cell<TaskId>> = Rc::new(Cell::new(TaskId::FINISHED));
    let a = spawn(protocol_job("A's", ida.clone(), true), 0, true);
    ida.set(a);
    let idb: Rc<Cell<TaskId>> = Rc::new(Cell::new(TaskId::FINISHED));
    let b = depend(a, protocol_job("B's", idb.clone(), true), 0, true, true);
    idb.set(b);
    let (c, seen) = context_seen_by_sync_dependent(b);
    wait(c);
    finish();
    assert_eq!(
        seen.get(),
        Some("B's"),
        "C ran after B's job closed its context"
    );
}

// --- Review AR-25 (fixes-3): a started pure task keeps the worker that
// started it until it has run.

#[test]
#[cfg_attr(miri, ignore)]
fn an_awaited_pure_task_waits_for_the_pure_tasks_in_front() {
    // tasks/wait_pure_queue_order: one worker, four pure tasks, the last
    // awaited first. The worker runs them in queue order, each to its end.
    start_test(1);
    let l = log();
    let ids: Vec<TaskId> = (0..4)
        .map(|k| spawn(job(&l, &k.to_string()), 0, false))
        .collect();
    wait(ids[3]);
    need_ok();
    assert_eq!(entries(&l), ["0", "1", "2", "3"]);
    finish();
    need_ok();
    assert_eq!(entries(&l), ["0", "1", "2", "3"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_pure_task_behind_a_started_one_can_still_be_deleted() {
    // tasks/drop_queued_behind_pure: the worker starts p0 during the sleep;
    // p1 stays queued behind it, so dropping p1 deletes it.
    start_test(1);
    let l = log();
    let p0 = spawn(job(&l, "p0"), 0, false);
    let flag = Rc::new(Cell::new(false));
    let d = DropFlag(flag.clone());
    let l2 = l.clone();
    let p1 = spawn(
        Box::new(move || {
            let _ = &d;
            l2.borrow_mut().push("p1".into());
            Outcome::Done
        }),
        0,
        false,
    );
    sleep_ms(1);
    assert_eq!(state(p0), TaskState::Running, "p0 started");
    need_ok();
    release(p1);
    assert!(flag.get(), "p1 was still queued: deleted");
    assert!(is_finished(p1));
    finish();
    need_ok();
    assert_eq!(entries(&l), ["p0"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_io_task_does_not_wait_for_started_pure_tasks() {
    // The exception LSCHED-01 keeps: the worker starts p0, p1 waits for it,
    // and the IO task queued behind both starts while `main` sleeps (a
    // runtime where it waited for p0 would hang a program that polls a
    // flag the IO task sets, as p0 is never needed).
    start_test(1);
    let l = log();
    let p0 = spawn(job(&l, "p0"), 0, false);
    let p1 = spawn(job(&l, "p1"), 0, false);
    let io = spawn(job(&l, "io"), 0, true);
    let late = sleep_or_stall(50);
    need_ok();
    if !late {
        assert_eq!(entries(&l), ["io"]);
        assert!(is_finished(io));
    }
    assert_eq!(state(p0), TaskState::Running, "started");
    assert_eq!(state(p1), TaskState::Waiting, "queued behind p0");
    finish();
    need_ok();
    let mut e = entries(&l);
    e.sort();
    assert_eq!(e, ["io", "p0", "p1"]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_waiter_runs_the_started_pure_tasks_it_waits_behind() {
    // A pending timer keeps the hub from its last resort; the waiter's need
    // runs the started pure task in front of the awaited one at once
    // (`needed_picked`), as natively its worker finishes it meanwhile.
    start_test(1);
    let l = log();
    let p0 = spawn(job(&l, "p0"), 0, false);
    sleep_ms(1);
    assert_eq!(state(p0), TaskState::Running, "p0 started");
    let p1 = spawn(job(&l, "p1"), 0, false);
    let t0 = std::time::Instant::now();
    let timer = timer_start(t0 + std::time::Duration::from_secs(20), Rc::new(|| {}));
    wait(p1);
    need_ok();
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(10),
        "the waiter did not wait for the timer"
    );
    assert_eq!(entries(&l), ["p0", "p1"]);
    assert!(timer_stop(timer));
    finish();
}

#[test]
#[cfg_attr(miri, ignore)]
fn wait_any_runs_the_started_pure_tasks_its_list_waits_behind() {
    // As above, for `IO.waitAny` (`wait_any_step`).
    start_test(1);
    let l = log();
    let p0 = spawn(job(&l, "p0"), 0, false);
    sleep_ms(1);
    assert_eq!(state(p0), TaskState::Running, "p0 started");
    let p1 = spawn(job(&l, "p1"), 0, false);
    let p2 = spawn(job(&l, "p2"), 0, false);
    let t0 = std::time::Instant::now();
    let timer = timer_start(t0 + std::time::Duration::from_secs(20), Rc::new(|| {}));
    assert_eq!(wait_any(&[p1, p2]), 0);
    need_ok();
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(10),
        "the waiter did not wait for the timer"
    );
    assert_eq!(entries(&l)[..2], ["p0", "p1"]);
    assert!(timer_stop(timer));
    finish();
}

#[test]
#[cfg_attr(miri, ignore)]
fn polling_runs_the_started_pure_tasks_the_polled_task_waits_behind() {
    // As above, for the polling threshold (`start_polled`): the polled pure
    // task finishes after a few sleeps, not never.
    start_test(1);
    let l = log();
    let p0 = spawn(job(&l, "p0"), 0, false);
    sleep_ms(1);
    assert_eq!(state(p0), TaskState::Running, "p0 started");
    let p1 = spawn(job(&l, "p1"), 0, false);
    let timer = timer_start(
        std::time::Instant::now() + std::time::Duration::from_secs(20),
        Rc::new(|| {}),
    );
    let mut sleeps = 0;
    while state(p1) != TaskState::Finished {
        assert!(sleeps < 100, "the polled task never ran");
        sleep_ms(1);
        sleeps += 1;
    }
    need_ok();
    assert_eq!(entries(&l), ["p0", "p1"]);
    assert!(timer_stop(timer));
    finish();
}

/// fixes-8 (lean2rr's RtTcp hang): the woken worker can start the awaited
/// pure task in the waiter's own look (`may_run_awaited`, its
/// `settle_worker`). `pick` then wakes the task's waiters, but this waiter
/// has not blocked yet. The task runs on the waiter's stack, as an awaited
/// started task does. Before the fix the waiter blocked on the started task
/// and no wake-up came: with a descriptor watched the hub never starts a
/// started pure task by itself (`last_resort`), so it waited in
/// `epoll_wait` forever. Here a timer ends the watch after 2 s, so the old
/// behaviour fails the test and does not hang it.
#[test]
#[cfg_attr(miri, ignore)]
fn a_pure_task_the_worker_starts_in_the_waiters_look_runs_there() {
    start_test(1);
    // A descriptor that never becomes readable is watched (in RtTcp, the
    // listening socket).
    let (r, w) = rustix::pipe::pipe().unwrap();
    let wid = watch(r, Interest::READ, Rc::new(|_| {})).unwrap();
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    let timer = timer_start(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
        Rc::new(move || {
            f2.set(true);
            unwatch(wid);
        }),
    );
    let ran_on = Rc::new(Cell::new(None));
    let r2 = ran_on.clone();
    // A pure task queued by `main`: the idle worker wakes (`enqueue`).
    let p = spawn(
        Box::new(move || {
            r2.set(Some(current_context()));
            Outcome::Done
        }),
        0,
        false,
    );
    // The worker's latency (90 µs) passes, and nothing looks at the
    // scheduler meanwhile: the worker starts `p` in `main`'s wait.
    std::thread::sleep(std::time::Duration::from_millis(2));
    wait(p);
    assert!(
        !fired.get(),
        "main blocked on the started task until the timer ended the watch"
    );
    assert_eq!(ran_on.get(), Some(MAIN), "p ran in main's wait");
    need_ok();
    assert!(timer_stop(timer));
    unwatch(wid);
    drop(w);
    finish();
}

/// As above, for `IO.waitAny`'s lone task (`wait_any_step`, review RF8-02).
/// Before fixes-8 the look said no, and the started task ran on a context
/// of its own.
#[test]
#[cfg_attr(miri, ignore)]
fn a_pure_task_the_worker_starts_in_wait_anys_look_runs_there() {
    start_test(1);
    let ran_on = Rc::new(Cell::new(None));
    let r2 = ran_on.clone();
    let p = spawn(
        Box::new(move || {
            r2.set(Some(current_context()));
            Outcome::Done
        }),
        0,
        false,
    );
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert_eq!(wait_any(&[p]), 0);
    assert_eq!(ran_on.get(), Some(MAIN), "p ran in main's IO.waitAny");
    need_ok();
    finish();
}

/// As above, for the root of the chain `main` waits for: `main` waits for
/// `p.map f`, and the worker starts `p` in the look (review RF8-02). No
/// descriptor is watched; a pending timer alone kept the hub from its last
/// resort, so before fixes-8 `main` waited until the timer had run
/// (review RF8-01).
#[test]
#[cfg_attr(miri, ignore)]
fn a_chain_root_the_worker_starts_in_the_waiters_look_runs_there() {
    start_test(1);
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    let timer = timer_start(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
        Rc::new(move || f2.set(true)),
    );
    let ran_on = Rc::new(Cell::new(None));
    let r2 = ran_on.clone();
    let p = spawn(
        Box::new(move || {
            r2.set(Some(current_context()));
            Outcome::Done
        }),
        0,
        false,
    );
    let m = depend(p, Box::new(|| Outcome::Done), 0, false, false);
    std::thread::sleep(std::time::Duration::from_millis(2));
    wait(m);
    assert!(!fired.get(), "main waited until the timer had run");
    assert_eq!(ran_on.get(), Some(MAIN), "p ran in main's wait");
    need_ok();
    assert!(timer_stop(timer));
    finish();
}

// --- Reviews HL2-01 to HL2-03 (fixes-13). HL2-01 needs a bind task that
// blocks on a context of its own while `main` waits for it, which these
// tests cannot make (no context suspends here): its regression tests are
// the case `tasks/wait_bind_continued_elsewhere` and the driver's program
// `hl2_bind_continued_waiter` (tests/sched-driver). So are HL2-02's and
// RF13-01..03's (a loop context that waits, carries a task or polls in a
// callback): `uvloop/loop_blocked_at_exit`, `loop_task_at_exit`,
// `loop_sleep_expired`, `loop_polls_at_exit` and the driver's program
// `rf13_loop_polls_at_exit`.

/// Review HL2-03 (`tasks/wait_chain_bind_continued`): `main` waits for
/// `u`, a dependent of `s2`, a `sync` dependent of the pure bind task `r`.
/// `wait` builds the chain `s2`, `r`, runs `r` on `main`'s stack, and `r`
/// continues as `t2` (`bind_wait`): `s2` waits again for a task that the
/// kept chain no longer holds. Before the fix `may_run_awaited` let `s2`
/// run there (in no queue) before `r` had finished: its `Task.get` of `r`
/// reported `GET_IN_SYNC_TASK`, and ran `t2` and `r` inside it. Now the
/// chain is built again (`t2`, `r`), and `s2` runs in `r`'s walk, once `r`
/// has finished. A pending timer keeps the hub from its last resort: with
/// only `may_run_awaited`'s half of the fix, `main` blocks on `s2` until
/// the timer has run (then the hub runs the started `t2`).
#[test]
#[cfg_attr(miri, ignore)]
fn a_kept_chain_runs_no_dependent_of_a_bind_task_that_continued() {
    start_test(4);
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    let timer = timer_start(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
        Rc::new(move || f2.set(true)),
    );
    let l = log();
    let reports: Rc<RefCell<Vec<String>>> = Rc::default();
    let l1 = l.clone();
    let r = spawn(
        Box::new(move || {
            l1.borrow_mut().push("r".into());
            let t2 = spawn(job(&l1, "t2"), 0, false);
            let l3 = l1.clone();
            Outcome::Continue(
                t2,
                Box::new(move || {
                    l3.borrow_mut().push("r copies t2".into());
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    let (l2, rep) = (l.clone(), reports.clone());
    let s2 = depend(
        r,
        Box::new(move || {
            l2.borrow_mut()
                .push(format!("s2: r finished {}", is_finished(r)));
            // `Task.map`'s function reads `r`'s value: the glue's
            // `Task.get` waits only while its slot is empty
            if !is_finished(r) {
                await_task(r, |m| rep.borrow_mut().push(m.to_owned()));
            }
            Outcome::Done
        }),
        0,
        true,
        false,
    );
    let u = depend(s2, job(&l, "u"), 0, false, false);
    need_ok();
    wait(u);
    need_ok();
    assert!(
        reports.borrow().is_empty(),
        "s2 ran as a sync task before r had finished: {:?}",
        reports.borrow()
    );
    assert_eq!(
        entries(&l),
        ["r", "t2", "r copies t2", "s2: r finished true", "u"]
    );
    assert!(!fired.get(), "main waited until the timer had run");
    assert!(timer_stop(timer));
    finish();
}

/// Review HL2-02: the final run does not wait for the event loop's context
/// once it waits in a callback (natively libuv's loop thread is detached;
/// the case `uvloop/loop_blocked_at_exit` checks it), but one that can go
/// on runs alone first, as before the fix (its budget, the final run's
/// task time plus 1 s, bounds it: reviews RF13-03 to RF13-07): here a
/// timer is due when `main` returns, and a look has started its loop
/// context, which has not run yet. Its callback runs in `finish`. (The look
/// is made by hand, as a hub step makes it while another context goes
/// first: since review HU-01 an effect point runs a due timer's callback at
/// once.)
#[test]
#[cfg_attr(miri, ignore)]
fn the_final_run_lets_a_loop_context_able_to_run_go_on() {
    start_test(2);
    let n = Rc::new(Cell::new(0));
    let n2 = n.clone();
    timer_start(
        std::time::Instant::now() + std::time::Duration::from_millis(1),
        Rc::new(move || n2.set(n2.get() + 1)),
    );
    std::thread::sleep(std::time::Duration::from_millis(5));
    // a look starts the loop context for the due timer
    with(|s| {
        s.ev_check(std::time::Instant::now(), true);
        s.ev_start_loop();
    });
    let lp = with(|s| s.ev.loop_ctx().map(|c| s.cx.ctxs[c].status));
    assert_eq!(
        lp,
        Some(ctx::Status::Runnable),
        "the loop context waits to run"
    );
    assert_eq!(n.get(), 0);
    finish();
    assert_eq!(n.get(), 1, "the loop context ran in the final run");
    assert!(with(|s| s.ev.loop_ctx().is_none()), "and ended");
}

// --- Review AR-27 (fixes-3): the per-task bookkeeping.

#[test]
fn the_slab_entry_is_56_bytes() {
    // docs/sched.md, "Per-task cost": it was 80 (a 16-byte `Option<Instant>`,
    // a 64-bit thread number, the priority in a byte of its own).
    assert_eq!(task::ENTRY_SIZE, 56);
}

#[test]
#[cfg_attr(miri, ignore)]
fn queue_times_survive_the_move_of_their_origin() {
    // `Sched::tick`: past 2^31 µs the origin moves on; a task queued before
    // the new origin is still one queued long ago (`Gate::Before`), and an
    // effect point lets it go first.
    start_test(2);
    let l = log();
    let old = spawn(job(&l, "old"), 0, true);
    // as if `old` had been queued 40 minutes ago
    with(|s| s.age_ticks_for_test(std::time::Duration::from_secs(40 * 60)));
    let fresh = spawn(job(&l, "fresh"), 0, true);
    with(|s| {
        assert_eq!(s.queued_at_of(old), Some(0), "before the new origin");
        assert_eq!(s.queued_at_of(fresh), Some(1 << 30));
    });
    effect();
    assert_eq!(entries(&l)[..1], ["old"], "a stale task goes first");
    finish();
    assert!(is_finished(old) && is_finished(fresh));
}

// --- Review AR-29 (fixes-3): `manager_running` is a thread-local flag.

#[test]
fn manager_running_follows_start() {
    assert!(!manager_running(), "no task manager before start");
    start_test(1);
    assert!(manager_running());
    with(|s| assert!(s.tk.started));
    start_test(0);
    assert!(!manager_running(), "LEAN_NUM_THREADS=0: no task manager");
    with(|s| assert!(!s.tk.started));
}

// --- Review AR-32 (fixes-3, lean2rr's AR-S3): `running_worker`.

thread_local! {
    /// What `WorkerGlue`'s hooks saw.
    static HOOKS_SAW: RefCell<Vec<(String, Option<u32>)>> = const { RefCell::new(Vec::new()) };
}

/// What each task saw: its tag and `running_worker()`.
type Seen = Rc<RefCell<Vec<(&'static str, Option<u32>)>>>;

/// A glue whose `task_begin` and `task_end` record `running_worker()`.
struct WorkerGlue;

impl Glue for WorkerGlue {
    fn suspend(&self, _: Suspend<'_>) {
        panic!("the crate's unit tests never suspend a context");
    }
    fn task_begin(&self, own: bool) {
        HOOKS_SAW.with(|h| {
            h.borrow_mut()
                .push((format!("begin {own}"), running_worker()))
        });
    }
    fn task_end(&self, own: bool) {
        HOOKS_SAW.with(|h| {
            h.borrow_mut()
                .push((format!("end {own}"), running_worker()))
        });
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn running_worker_names_a_pool_tasks_emulated_worker() {
    assert_eq!(running_worker(), None, "the initializers");
    start_with(Rc::new(WorkerGlue), 1, 1 << 20);
    assert_eq!(running_worker(), None, "main");
    let seen: Seen = Rc::default();
    let rec = |tag: &'static str| -> Job {
        let s = seen.clone();
        Box::new(move || {
            s.borrow_mut().push((tag, running_worker()));
            Outcome::Done
        })
    };
    let a = spawn(rec("a"), 0, true);
    // a `sync` dependent of `a`, run in its walk, on its thread
    depend(a, rec("sync"), 0, true, true);
    let b = spawn(rec("b"), 0, false);
    wait(a);
    wait(b);
    let d = spawn(rec("dedicated"), 9, true);
    wait(d);
    assert_eq!(running_worker(), None, "main again");
    // two pool tasks on the one worker: one id; a `sync` dependent shares
    // its thread's (review RF3-03); a dedicated task: none
    assert_eq!(
        *seen.borrow(),
        [
            ("a", Some(0)),
            ("sync", Some(0)),
            ("b", Some(0)),
            ("dedicated", None)
        ]
    );
    // the glue's hooks see the task's own, also after its walk
    let saw = HOOKS_SAW.with(|h| h.borrow().clone());
    let want: Vec<(String, Option<u32>)> = [
        ("begin true", Some(0)),
        ("begin false", Some(0)),
        ("end false", Some(0)),
        ("end true", Some(0)),
        ("begin true", Some(0)),
        ("end true", Some(0)),
        ("begin true", None),
        ("end true", None),
    ]
    .iter()
    .map(|&(s, w)| (s.to_string(), w))
    .collect();
    assert_eq!(saw, want);
    finish();
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_pool_task_run_by_a_pool_waiter_takes_the_next_id() {
    // `a` waits for `c`, which runs on `a`'s stack: `a` still holds its
    // worker id (natively its thread waits in `Task.get`), so `c` takes the
    // next one; after both, the lowest is free again.
    //
    // `a` is spawned before `c` (review AR-38). The lone worker, woken by the
    // first enqueue, starts the head of the highest non-empty queue once
    // `LATENCY_COLD` (90 µs) has passed (`settle_worker`). With `c` spawned
    // first, a stall of 90 µs before `a`'s spawn let the worker start `c`
    // first, as a native worker would: then `c` ran before `a`, on the
    // worker's id. With `a` first, `a` is the head of the highest queue from
    // its spawn on (priority 8, and queued first), so `a` runs first whether
    // the worker starts it at `c`'s spawn or at `main`'s wait. No task runs
    // before `main`'s wait, so `c`'s id is set when `a` reads it.
    start_test(1);
    let seen: Seen = Rc::default();
    let s1 = seen.clone();
    let s2 = seen.clone();
    let cid: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let cid2 = cid.clone();
    let a = spawn(
        Box::new(move || {
            s1.borrow_mut().push(("a before", running_worker()));
            wait(cid2.get().expect("c is spawned before any task runs"));
            s1.borrow_mut().push(("a after", running_worker()));
            Outcome::Done
        }),
        8,
        true,
    );
    let c = spawn(
        Box::new(move || {
            s2.borrow_mut().push(("c", running_worker()));
            Outcome::Done
        }),
        0,
        true,
    );
    cid.set(Some(c));
    wait(a);
    let s3 = seen.clone();
    let e = spawn(
        Box::new(move || {
            s3.borrow_mut().push(("e", running_worker()));
            Outcome::Done
        }),
        0,
        true,
    );
    wait(e);
    assert_eq!(
        *seen.borrow(),
        [
            ("a before", Some(0)),
            ("c", Some(1)),
            ("a after", Some(0)),
            ("e", Some(0))
        ]
    );
    finish();
}

// --- Review RF3-01 (fixes-3): a started pure task keeps the worker id of
// the worker that started it, from the pick on.

/// Two workers; pure task `p` is started (picked) by a free worker, then IO
/// task `x` runs on the other worker, sets its stdout and does not restore
/// it. Natively `p` runs on the worker that took it, with that worker's
/// (fresh) streams. Before the fix `p` took the lowest free id when it ran,
/// whose set `x` had used: `x`'s leftover stdout, and `x`'s id.
#[cfg(feature = "io")]
#[test]
#[cfg_attr(miri, ignore)]
fn a_started_pure_task_runs_with_its_workers_streams() {
    use crate::io::streams::{self, StdStream};
    fn out() -> u32 {
        streams::current(StdStream::Stdout, || 0u32)
    }
    start_test(2);
    // what `p` saw: its stdout and its worker id
    type Saw = Option<(u32, Option<u32>)>;
    let p_saw: Rc<Cell<Saw>> = Rc::default();
    let x_saw: Rc<Cell<Option<u32>>> = Rc::default();
    let (p2, x2) = (p_saw.clone(), x_saw.clone());
    let p = spawn(
        Box::new(move || {
            p2.set(Some((out(), running_worker())));
            Outcome::Done
        }),
        0,
        false,
    );
    // `main` yields: a free worker starts `p` (a pick)
    sleep_ms(1);
    assert_eq!(state(p), TaskState::Running, "p started");
    let x = spawn(
        Box::new(move || {
            x2.set(running_worker());
            let _ = streams::set_stdout(5u32, || 0);
            Outcome::Done
        }),
        0,
        true,
    );
    wait(x);
    wait(p);
    need_ok();
    finish();
    assert_eq!(x_saw.get(), Some(1), "x on the other worker");
    assert_eq!(
        p_saw.get(),
        Some((0, Some(0))),
        "p ran on the worker that started it, with its streams"
    );
}

/// Review RF3-03: a `sync` dependent run on `main`'s thread (`resolve`
/// there) shares `main`'s answer, `None`, as in threads mode.
#[test]
#[cfg_attr(miri, ignore)]
fn a_sync_dependent_shares_its_threads_worker() {
    start_test(1);
    let seen: Seen = Rc::default();
    let s1 = seen.clone();
    let p = promise_new().unwrap();
    depend(
        p,
        Box::new(move || {
            s1.borrow_mut().push(("sync on main", running_worker()));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    resolve(p, || {});
    assert_eq!(*seen.borrow(), [("sync on main", None)]);
    finish();
}

// --- Review AR-37 (fixes-5, lean2rr's RS5-04): `tid_offset`, the OS
// thread `IO.getTID` names.

/// Natively a pool worker stays alive and takes the next pool task once
/// idle, and a dedicated task always gets a new thread. Before the fix the
/// number was `thread_number`, the depth on the context: every task below
/// that runs on `main`'s stack got 1, so a dedicated task that followed a
/// pool task had the pool task's id, and the tasks the waiter ran 2.
#[test]
#[cfg_attr(miri, ignore)]
fn tid_offset_tells_a_dedicated_task_from_the_idle_worker() {
    start_test(1);
    assert_eq!(tid_offset(), 0, "main");
    let seen: Rc<RefCell<Vec<(&'static str, u64)>>> = Rc::default();
    let rec = |tag: &'static str| -> Job {
        let s = seen.clone();
        Box::new(move || {
            s.borrow_mut().push((tag, tid_offset()));
            Outcome::Done
        })
    };
    // one after the other: pool (with a `sync` dependent, on its thread),
    // dedicated, pool, dedicated
    let a = spawn(rec("pool a"), 0, true);
    depend(a, rec("sync after a"), 0, true, true);
    wait(a);
    for (tag, prio) in [("dedicated d", 9), ("pool b", 0), ("dedicated e", 9)] {
        let t = spawn(rec(tag), prio, true);
        wait(t);
    }
    // a pool task that makes and waits for a pool task, then a dedicated
    // task: the first takes the next worker (natively a new one, since the
    // waiter's thread waits in `Task.get`), the second a new thread; the
    // waiter has its own number again after them
    let (s, rc, ry) = (
        seen.clone(),
        rec("pool c, waited for"),
        rec("dedicated y, waited for"),
    );
    let x = spawn(
        Box::new(move || {
            s.borrow_mut().push(("pool x before", tid_offset()));
            wait(spawn(rc, 0, true));
            wait(spawn(ry, 9, true));
            s.borrow_mut().push(("pool x after", tid_offset()));
            Outcome::Done
        }),
        0,
        true,
    );
    wait(x);
    // a `sync` dependent of a promise that `main` resolves: `main`'s thread
    let p = promise_new().unwrap();
    depend(p, rec("sync on main"), 0, true, true);
    resolve(p, || {});
    assert_eq!(tid_offset(), 0, "main again");
    assert_eq!(
        *seen.borrow(),
        [
            ("pool a", 1),
            ("sync after a", 1),
            ("dedicated d", 2),
            ("pool b", 1),
            ("dedicated e", 3),
            ("pool x before", 1),
            ("pool c, waited for", 4),
            ("dedicated y, waited for", 5),
            ("pool x after", 1),
            ("sync on main", 0),
        ]
    );
    finish();
}

/// The event loop's callbacks, and the `sync` dependents they run, are on
/// native's one loop thread (`libuv.cpp` 26): one number for every loop
/// context (a loop context ends once no callback is due, so each timer
/// below runs on a new one), neither `main`'s nor a worker's. Before, each
/// loop context had a number of its own.
#[test]
#[cfg_attr(miri, ignore)]
fn tid_offset_of_the_event_loop_is_one_thread() {
    start_test(1);
    let seen: Rc<RefCell<Vec<(&'static str, u64)>>> = Rc::default();
    let s = seen.clone();
    wait(spawn(
        Box::new(move || {
            s.borrow_mut().push(("pool", tid_offset()));
            Outcome::Done
        }),
        0,
        true,
    ));
    for (tag, dep) in [("timer 1", "sync after 1"), ("timer 2", "sync after 2")] {
        let p = promise_new().unwrap();
        let s = seen.clone();
        depend(
            p,
            Box::new(move || {
                s.borrow_mut().push((dep, tid_offset()));
                Outcome::Done
            }),
            0,
            true,
            true,
        );
        let s = seen.clone();
        let cb: Rc<dyn Fn()> = Rc::new(move || {
            s.borrow_mut().push((tag, tid_offset()));
            resolve(p, || {});
        });
        timer_start(
            std::time::Instant::now() + std::time::Duration::from_millis(5),
            cb,
        );
        wait(p);
    }
    assert_eq!(tid_offset(), 0, "main");
    assert_eq!(
        *seen.borrow(),
        [
            ("pool", 1),
            ("timer 1", 2),
            ("sync after 1", 2),
            ("timer 2", 2),
            ("sync after 2", 2),
        ]
    );
    finish();
}

// --- Hunt HSG-01 (fixes-18): the owner of a lock is the emulated OS thread
// `IO.getTID` names (`tid_offset`), not the context and the depth of the
// innermost task on it (`thread_number`).

/// A job that locks `m` and ends with it locked, as a task that returns
/// with a lock held: natively its thread keeps the lock.
fn locking(m: &Rc<RecursiveMutex>) -> Job {
    let m = m.clone();
    Box::new(move || {
        m.lock();
        Outcome::Done
    })
}

/// The `try_lock` results the tests below saw, each with its tag.
type Tries = Rc<RefCell<Vec<(&'static str, bool)>>>;

/// A job that records `m.try_lock()` under `tag`.
fn trying(m: &Rc<RecursiveMutex>, seen: &Tries, tag: &'static str) -> Job {
    let (m, seen) = (m.clone(), seen.clone());
    Box::new(move || {
        seen.borrow_mut().push((tag, m.try_lock()));
        Outcome::Done
    })
}

/// Natively a dedicated task always gets a new thread
/// (`spawn_dedicated_worker`), so a recursive mutex an earlier dedicated
/// task left locked is another thread's. Before HSG-01 both ran at depth 1
/// on `main`'s stack, one owner: `tryLock` was true.
#[test]
#[cfg_attr(miri, ignore)]
fn a_dedicated_task_after_one_that_left_a_lock_waits() {
    start_test(1);
    let m = Rc::new(RecursiveMutex::new());
    let seen: Tries = Rc::default();
    wait(spawn(locking(&m), 9, true));
    wait(spawn(trying(&m, &seen, "dedicated"), 9, true));
    assert!(!m.try_lock(), "main is another thread");
    assert_eq!(*seen.borrow(), [("dedicated", false)]);
    finish();
}

/// A pool task runs on a worker thread, and a dedicated task after it on a
/// new thread: the mutex the pool task left locked is another thread's.
/// Before HSG-01, one owner (depth 1 on `main`'s stack): `tryLock` true.
#[test]
#[cfg_attr(miri, ignore)]
fn a_dedicated_task_after_a_pool_task_that_left_a_lock_waits() {
    start_test(1);
    let m = Rc::new(RecursiveMutex::new());
    let seen: Tries = Rc::default();
    wait(spawn(locking(&m), 0, true));
    wait(spawn(trying(&m, &seen, "dedicated"), 9, true));
    assert!(!m.try_lock(), "main is another thread");
    assert_eq!(*seen.borrow(), [("dedicated", false)]);
    finish();
}

/// Natively a pool worker stays alive and takes the next pool task once
/// idle (`enqueue_core` wakes the idle one), so a pool task after a pool
/// task that ended with a recursive mutex locked locks it again: the same
/// thread. Two shapes: each task started while `main` sleeps (the hunt's
/// program, where each ran on a context of its own: before HSG-01 two
/// owners, `tryLock` false, and `lock` hung), and one run for a dedicated
/// task that waits for it (natively the idle worker runs it; here one
/// level deeper on the waiter's stack, so before HSG-01 another owner).
/// `main` and a dedicated task are other threads.
#[test]
#[cfg_attr(miri, ignore)]
fn a_pool_task_on_the_idle_worker_locks_again() {
    start_test(1);
    let m = Rc::new(RecursiveMutex::new());
    let seen: Tries = Rc::default();
    let p = spawn(locking(&m), 0, true);
    sleep_ms(20);
    wait(p);
    let q = spawn(trying(&m, &seen, "pool, after a sleep"), 0, true);
    sleep_ms(20);
    wait(q);
    let r = trying(&m, &seen, "pool, for a dedicated waiter");
    wait(spawn(
        Box::new(move || {
            wait(spawn(r, 0, true));
            Outcome::Done
        }),
        9,
        true,
    ));
    wait(spawn(trying(&m, &seen, "dedicated"), 9, true));
    assert!(!m.try_lock(), "main is another thread");
    assert_eq!(
        *seen.borrow(),
        [
            ("pool, after a sleep", true),
            ("pool, for a dedicated waiter", true),
            ("dedicated", false),
        ]
    );
    finish();
}

/// Two pool tasks that run at once never share a thread: a pool task that
/// holds a recursive mutex and waits for another pool task (natively its
/// worker waits in `Task.get`, and another worker runs that task) keeps its
/// emulated worker, and the task run on its stack takes another one,
/// directly or through a dedicated task that waits for it. The holder
/// itself locks again.
#[test]
#[cfg_attr(miri, ignore)]
fn pool_tasks_that_run_at_once_never_share_an_owner() {
    start_test(1);
    let m = Rc::new(RecursiveMutex::new());
    let seen: Tries = Rc::default();
    let (m2, s2) = (m.clone(), seen.clone());
    let a = spawn(
        Box::new(move || {
            m2.lock();
            wait(spawn(trying(&m2, &s2, "pool, for the holder"), 0, true));
            let c = trying(&m2, &s2, "pool, for a dedicated task");
            wait(spawn(
                Box::new(move || {
                    wait(spawn(c, 0, true));
                    Outcome::Done
                }),
                9,
                true,
            ));
            s2.borrow_mut().push(("the holder", m2.try_lock()));
            Outcome::Done
        }),
        0,
        true,
    );
    wait(a);
    assert_eq!(
        *seen.borrow(),
        [
            ("pool, for the holder", false),
            ("pool, for a dedicated task", false),
            ("the holder", true),
        ]
    );
    finish();
}

/// A `sync` task runs on the thread below it, natively and here: a
/// dependent that `depend` runs at once (`FAST`, Lean's fast path) on its
/// caller's, a `sync` dependent on the thread that finished its source or
/// resolved its promise. So each locks again what that thread holds; a
/// `sync` dependent of a promise that a pool task resolves does not lock
/// again what `main` holds.
#[test]
#[cfg_attr(miri, ignore)]
fn a_sync_task_owns_as_the_thread_below_it() {
    start_test(1);
    let seen: Tries = Rc::default();
    // a pool task that holds the mutex: a dependent run at once in it, and
    // a `sync` dependent of it (made by `main` before it runs)
    let m = Rc::new(RecursiveMutex::new());
    let (m2, s2) = (m.clone(), seen.clone());
    let a = spawn(
        Box::new(move || {
            m2.lock();
            depend(
                TaskId::FINISHED,
                trying(&m2, &s2, "at once, in the holder"),
                0,
                true,
                true,
            );
            Outcome::Done
        }),
        0,
        true,
    );
    depend(a, trying(&m, &seen, "in the holder's walk"), 0, true, true);
    wait(a);
    // `main` holds the mutex: a dependent run at once on it, a `sync`
    // dependent of a promise it resolves, and one of a promise that a pool
    // task resolves
    let n = Rc::new(RecursiveMutex::new());
    n.lock();
    depend(
        TaskId::FINISHED,
        trying(&n, &seen, "at once, on main"),
        0,
        true,
        true,
    );
    let p = promise_new().unwrap();
    depend(p, trying(&n, &seen, "of main's resolution"), 0, true, true);
    resolve(p, || {});
    let q = promise_new().unwrap();
    depend(
        q,
        trying(&n, &seen, "of a pool task's resolution"),
        0,
        true,
        true,
    );
    wait(spawn(
        Box::new(move || {
            resolve(q, || {});
            Outcome::Done
        }),
        0,
        true,
    ));
    assert_eq!(
        *seen.borrow(),
        [
            ("at once, in the holder", true),
            ("in the holder's walk", true),
            ("at once, on main", true),
            ("of main's resolution", true),
            ("of a pool task's resolution", false),
        ]
    );
    finish();
}

/// The event loop's callbacks run on native's one loop thread (`libuv.cpp`
/// 26), here on a new loop context each time one becomes due after the
/// last has ended: a mutex one callback left locked, a later callback locks
/// again, and so does the `sync` dependent of the promise it resolves.
/// `main` and a pool task are other threads. Before HSG-01 each loop
/// context was another owner.
#[test]
#[cfg_attr(miri, ignore)]
fn loop_callbacks_own_as_one_thread() {
    start_test(1);
    let m = Rc::new(RecursiveMutex::new());
    let seen: Tries = Rc::default();
    let soon = || std::time::Instant::now() + std::time::Duration::from_millis(5);
    let p = promise_new().unwrap();
    let m1 = m.clone();
    let cb: Rc<dyn Fn()> = Rc::new(move || {
        m1.lock();
        resolve(p, || {});
    });
    timer_start(soon(), cb);
    wait(p);
    let q = promise_new().unwrap();
    depend(
        q,
        trying(&m, &seen, "sync dependent of a callback"),
        0,
        true,
        true,
    );
    let (m2, s2) = (m.clone(), seen.clone());
    let cb: Rc<dyn Fn()> = Rc::new(move || {
        s2.borrow_mut().push(("a later callback", m2.try_lock()));
        resolve(q, || {});
    });
    timer_start(soon(), cb);
    wait(q);
    assert!(!m.try_lock(), "main is another thread");
    wait(spawn(trying(&m, &seen, "pool"), 0, true));
    assert_eq!(
        *seen.borrow(),
        [
            ("a later callback", true),
            ("sync dependent of a callback", true),
            ("pool", false),
        ]
    );
    finish();
}

// --- Review AR-34 (fixes-4): the standard workers end before the dedicated
// tasks are waited for.

thread_local! {
    /// What `EndGlue` and the tasks of the tests below saw.
    static END_LOG: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

fn end_log(s: &'static str) {
    END_LOG.with(|l| l.borrow_mut().push(s));
}

/// A glue that records `workers_end`.
struct EndGlue;

impl Glue for EndGlue {
    fn suspend(&self, _: Suspend<'_>) {
        panic!("the crate's unit tests never suspend a context");
    }
    fn workers_end(&self) {
        end_log("workers_end");
    }
}

fn logging(s: &'static str) -> Job {
    Box::new(move || {
        end_log(s);
        Outcome::Done
    })
}

#[test]
fn the_workers_end_once_before_the_dedicated_tasks_run() {
    start_with(Rc::new(EndGlue), 1, 1 << 20);
    let pool = spawn(logging("pool"), 0, true);
    wait(pool);
    // a dedicated task still to run at `finish`, which makes a pool task
    // (LB-13's corrected run: it runs after the workers ended)
    spawn(
        Box::new(|| {
            end_log("dedicated");
            spawn(logging("late pool"), 0, true);
            Outcome::Done
        }),
        9,
        true,
    );
    finish();
    let log = END_LOG.with(|l| l.borrow().clone());
    assert_eq!(log, ["pool", "workers_end", "dedicated", "late pool"]);
}

/// With `io`: a pool task that runs after the workers ended drops its
/// streams at its end (natively, on a worker made after the others ended,
/// its thread finalizers would drop them).
#[cfg(feature = "io")]
#[test]
fn a_late_pool_task_drops_its_streams_at_its_end() {
    use crate::io::streams;
    start_with(Rc::new(EndGlue), 1, 1 << 20);
    let flag = Rc::new(Cell::new(false));
    let f2 = flag.clone();
    spawn(
        Box::new(move || {
            let f3 = f2.clone();
            spawn(
                Box::new(move || {
                    let s = Rc::new(DropFlag(f3));
                    let _ = streams::set_stdout(s, || Rc::new(DropFlag(Rc::default())));
                    Outcome::Done
                }),
                0,
                true,
            );
            Outcome::Done
        }),
        9,
        true,
    );
    finish();
    assert!(
        flag.get(),
        "the late pool task's stdout was dropped at its end"
    );
}

/// Review HR-02 (fixes-14): `depend` with a source that has finished by the
/// time the dependent is made (the glue asked `dependent_runs_now` before
/// `depend`'s writers point let it finish) runs a `sync` dependent at once,
/// inside the call, as Lean's fast path does (the function applied in the
/// caller: no `sync` task, review RF14-03); an async one is queued. Before
/// the fix the `sync` one was queued too.
#[test]
#[cfg_attr(miri, ignore)]
fn depend_runs_a_sync_dependent_of_a_finished_source_at_once() {
    start_test(1);
    let ran = Rc::new(Cell::new(None));
    let r2 = ran.clone();
    let id = depend(
        TaskId::FINISHED,
        Box::new(move || {
            r2.set(Some(in_sync_task()));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    assert_eq!(
        ran.get(),
        Some(false),
        "ran inside depend, as the caller's code"
    );
    assert!(is_finished(id));
    let l = log();
    let id2 = depend(TaskId::FINISHED, job(&l, "async"), 0, false, true);
    assert!(entries(&l).is_empty());
    assert!(!is_finished(id2));
    finish();
    assert_eq!(entries(&l), ["async"]);
}

/// Review HR-02's bind half: a `sync` bind task whose function returned a
/// task that has finished by the time the bind task waits for it (the
/// writers point at the job's end let it finish after the glue's check)
/// runs on at once on the same thread, as `add_dep`'s `enqueue_core` runs
/// a `LEAN_SYNC_PRIO` task; an async bind task is queued. Before the fix
/// the `sync` one was queued.
#[test]
#[cfg_attr(miri, ignore)]
fn a_sync_bind_task_whose_task_has_finished_runs_on_at_once() {
    start_test(1);
    let p = promise_new().unwrap();
    let ran = Rc::new(Cell::new(None));
    let r2 = ran.clone();
    let b = depend(
        p,
        Box::new(move || {
            Outcome::Continue(
                TaskId::FINISHED,
                Box::new(move || {
                    r2.set(Some(in_sync_task()));
                    Outcome::Done
                }),
            )
        }),
        0,
        true,
        true,
    );
    let l = log();
    let l2 = l.clone();
    let a = depend(
        p,
        Box::new(move || {
            Outcome::Continue(
                TaskId::FINISHED,
                Box::new(move || {
                    l2.borrow_mut().push("async continuation".into());
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
        true,
    );
    resolve(p, || {});
    // the bind task ran in the promise's walk, and its continuation right
    // after it, on the same thread
    assert_eq!(ran.get(), Some(true));
    assert!(is_finished(b));
    assert!(!is_finished(a));
    finish();
    assert_eq!(entries(&l), ["async continuation"]);
}

/// Review RF14-03: the function of a `sync` dependent that `depend` runs at
/// once is the caller's code, as in Lean's fast path: a wait in it for an
/// unfinished task reports no `GET_IN_SYNC_TASK` when the caller is no
/// `sync` task (here `main`), and reports it when the caller is one (here a
/// promise's `sync` dependent), as natively `wait_for` sees the caller's
/// task.
#[test]
#[cfg_attr(miri, ignore)]
fn a_dependent_run_at_once_waits_as_its_caller() {
    start_test(1);
    let reports = Rc::new(RefCell::new(Vec::new()));
    let job_waiting = |reports: &Rc<RefCell<Vec<String>>>| -> Job {
        let reports = reports.clone();
        Box::new(move || {
            let other = spawn(Box::new(|| Outcome::Done), 0, true);
            await_task(other, |m| reports.borrow_mut().push(m.to_string()));
            Outcome::Done
        })
    };
    let id = depend(TaskId::FINISHED, job_waiting(&reports), 0, true, true);
    assert!(is_finished(id));
    assert!(reports.borrow().is_empty(), "{:?}", reports.borrow());
    let p = promise_new().unwrap();
    let r2 = reports.clone();
    let _d = depend(
        p,
        Box::new(move || {
            depend(TaskId::FINISHED, job_waiting(&r2), 0, true, true);
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    resolve(p, || {});
    assert_eq!(*reports.borrow(), [GET_IN_SYNC_TASK.to_string()]);
    finish();
}

/// Review RF14-03's gap: the function of a dependent that `depend` runs at
/// once is the caller's code, so `IO.checkCanceled` answers with the
/// caller's flag: here a pool task that canceled itself.
#[test]
#[cfg_attr(miri, ignore)]
fn a_dependent_run_at_once_checks_its_callers_cancel() {
    start_test(1);
    let me = Rc::new(Cell::new(TaskId::FINISHED));
    let seen = Rc::new(Cell::new(None));
    let (m2, s2) = (me.clone(), seen.clone());
    let t = spawn(
        Box::new(move || {
            cancel(m2.get());
            let s3 = s2.clone();
            depend(
                TaskId::FINISHED,
                Box::new(move || {
                    s3.set(Some(check_canceled()));
                    Outcome::Done
                }),
                0,
                true,
                true,
            );
            Outcome::Done
        }),
        0,
        true,
    );
    me.set(t);
    wait(t);
    assert_eq!(seen.get(), Some(true), "the caller's cancel");
    finish();
}

/// Review AR-53: after `main` returned, the function of a dependent that
/// `depend` runs at once is its caller's code for the shutdown flag too: once
/// the caller (a task queued when `main` returned) has seen the flag, so has
/// the function, and a task it creates then, or a promise's dependent it
/// releases, could only start after the flag was set, and sees it at once.
/// One worker, which the caller holds, so no check here starts a task on a
/// context of its own; the promise's dependent is a `sync` one, run inside
/// the resolve.
#[test]
#[cfg_attr(miri, ignore)]
fn a_dependent_run_at_once_is_late_once_its_caller_checked() {
    start_test(1);
    let l = log();
    let l2 = l.clone();
    let p = promise_new().unwrap();
    spawn(
        Box::new(move || {
            let l3 = l2.clone();
            depend(
                TaskId::FINISHED,
                Box::new(move || {
                    let first = check_canceled();
                    l3.borrow_mut().push(format!("caller {first}"));
                    let l4 = l3.clone();
                    spawn(
                        Box::new(move || {
                            let seen = check_canceled();
                            l4.borrow_mut().push(format!("created {seen}"));
                            Outcome::Done
                        }),
                        0,
                        true,
                    );
                    let l5 = l3.clone();
                    depend(
                        p,
                        Box::new(move || {
                            let seen = check_canceled();
                            l5.borrow_mut().push(format!("released {seen}"));
                            Outcome::Done
                        }),
                        0,
                        true,
                        true,
                    );
                    resolve(p, || {});
                    Outcome::Done
                }),
                0,
                true,
                true,
            );
            Outcome::Done
        }),
        0,
        true,
    );
    finish();
    assert_eq!(
        entries(&l),
        ["caller false", "released true", "created true"]
    );
}

/// Review RF14-04: a `sync` bind task whose function returned a task that
/// has finished runs on at once on the thread of its first run, not on the
/// thread below it now: here its source's, a pool task run on `main`'s
/// stack, whose walk ran the bind task.
#[test]
#[cfg_attr(miri, ignore)]
fn a_sync_bind_task_runs_on_on_the_thread_of_its_first_run() {
    start_test(1);
    let threads = Rc::new(RefCell::new(Vec::new()));
    let src = spawn(Box::new(|| Outcome::Done), 0, true);
    let t2 = threads.clone();
    let _b = depend(
        src,
        Box::new(move || {
            t2.borrow_mut().push(thread_number());
            let t3 = t2.clone();
            Outcome::Continue(
                TaskId::FINISHED,
                Box::new(move || {
                    t3.borrow_mut().push(thread_number());
                    Outcome::Done
                }),
            )
        }),
        0,
        true,
        true,
    );
    wait(src);
    let t = threads.borrow().clone();
    assert_eq!(t.len(), 2, "{t:?}");
    assert_ne!(t[0], 0, "the bind task ran on its source's thread");
    assert_eq!(t[1], t[0], "and its continuation on the same");
    finish();
}

/// Review RF15-A01 (threads mode's hunt HMT-04, here in the single-thread
/// scheduler): a pure bind task released while it runs, which a dependent
/// still holds (`deactivate`'s held rule: it runs on as a started one,
/// deleted and canceled), continues as an unreleased one when its function
/// returns `Continue`: its continuation runs once the task it continues as
/// has finished, and its dependent gets its value and inherits its cancel.
/// Before the fix `bind_wait` freed its entry with the dependent still
/// linked (the debug assertion of `free_entry`) and dropped the
/// continuation. One worker; everything runs on `main`'s stack (`wait` of
/// the dependent): the bind task `b`, then `x`, which it continues as, then
/// `b`'s continuation, then the dependent.
#[test]
#[cfg_attr(miri, ignore)]
fn hmt_04_a_held_released_bind_task_continues() {
    start_test(1);
    let l = log();
    let me: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let next: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let out = Rc::new(Cell::new(0u32));
    let (me2, next2, out2, l2) = (me.clone(), next.clone(), out.clone(), l.clone());
    let b = spawn(
        Box::new(move || {
            // the translator's last reference goes while it runs
            release(me2.get().unwrap());
            l2.borrow_mut().push("b".to_string());
            Outcome::Continue(
                next2.get().unwrap(),
                Box::new(move || {
                    out2.set(42);
                    l2.borrow_mut().push("k".to_string());
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    me.set(Some(b));
    let (out3, l3) = (out.clone(), l.clone());
    let d = depend(
        b,
        Box::new(move || {
            let canceled = check_canceled();
            l3.borrow_mut().push(format!("d {} {canceled}", out3.get()));
            Outcome::Done
        }),
        0,
        false,
        true,
    );
    let x = spawn(job(&l, "x"), 0, true);
    next.set(Some(x));
    wait(d);
    assert_eq!(entries(&l), ["b", "x", "k", "d 42 true"]);
    finish();
}

/// `switch_is_event_loop` is false outside the hub's `switched` (lean2rr's
/// glue asks inside it, hunt HST-01; the loop's switches are the drivers'
/// to show).
#[test]
fn switch_is_event_loop_is_false_outside_switched() {
    start_test(1);
    assert!(!switch_is_event_loop());
    finish();
}

/// A glue that logs each task's end (`task_end`) and suspends nothing.
struct EndLog(Log);

impl Glue for EndLog {
    fn suspend(&self, _: Suspend<'_>) {
        panic!("the crate's unit tests never suspend a context");
    }
    fn task_end(&self, own_thread: bool) {
        self.0.borrow_mut().push(format!("task_end {own_thread}"));
    }
}

/// Hunt HST-04: a deleted bind task's continuation is dropped before the
/// glue's `task_end`, while the task's emulated thread is still the running
/// one: natively `run_task` frees a deleted task on the worker's thread, so
/// the code its drop runs (a promise's resolution and its `sync`
/// dependents) sees that thread's state, such as the worker's current
/// streams, which a glue gives back at `task_end`. Before, the drop came
/// after `task_end`. The pure bind task releases itself while it runs (its
/// translator's last reference), then returns `Continue`; `finish` runs it.
#[test]
#[cfg_attr(miri, ignore)]
fn a_deleted_bind_tasks_continuation_is_dropped_before_task_end() {
    struct OnDrop(Log);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.borrow_mut().push("continuation dropped".to_string());
        }
    }
    let l = log();
    start_with(Rc::new(EndLog(l.clone())), 1, 1 << 20);
    let p = promise_new().unwrap();
    let me: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let (me2, l2) = (me.clone(), l.clone());
    let b = spawn(
        Box::new(move || {
            release(me2.get().unwrap());
            let keep = OnDrop(l2);
            Outcome::Continue(
                p,
                Box::new(move || {
                    let _keep = &keep;
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    me.set(Some(b));
    finish();
    let seen = entries(&l);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], "continuation dropped", "{seen:?}");
    assert!(seen[1].starts_with("task_end"), "{seen:?}");
}

/// Review RF15-C01: a panic in a deleted bind task's
/// continuation's drop, caught on `main`, must still give the glue its
/// `task_end` (RS1S-12: "Either way the glue hears of the task's end").
/// Before the fix the guard was forgotten before the drop (since HST-04's
/// reorder), and the log was `["begin true"]`.
#[test]
#[cfg_attr(miri, ignore)]
fn a_panic_in_a_deleted_continuations_drop_ends_the_task() {
    struct BeginEnd(Log);
    impl Glue for BeginEnd {
        fn suspend(&self, _: Suspend<'_>) {
            panic!("the crate's unit tests never suspend a context");
        }
        fn task_begin(&self, own: bool) {
            self.0.borrow_mut().push(format!("begin {own}"));
        }
        fn task_end(&self, own: bool) {
            self.0.borrow_mut().push(format!("end {own}"));
        }
    }
    struct Boom;
    impl Drop for Boom {
        fn drop(&mut self) {
            panic!("boom in a continuation's drop");
        }
    }
    let l = log();
    start_with(Rc::new(BeginEnd(l.clone())), 1, 1 << 20);
    let p = promise_new().unwrap();
    let me: Rc<Cell<Option<TaskId>>> = Rc::new(Cell::new(None));
    let me2 = me.clone();
    let b = spawn(
        Box::new(move || {
            release(me2.get().unwrap());
            let boom = Boom;
            Outcome::Continue(
                p,
                Box::new(move || {
                    let _b = &boom;
                    Outcome::Done
                }),
            )
        }),
        0,
        false,
    );
    me.set(Some(b));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait(b)));
    assert!(r.is_err(), "the drop's panic reaches main");
    assert_eq!(entries(&l), ["begin true", "end true"]);
}

/// Review RF15-C02: a `sync` dependent that a promise's walk
/// runs on `main`'s stack, for a promise `main` resolved, runs on `main`'s
/// thread natively (`LEAN_SYNC_PRIO`, `enqueue_core` runs it there), as a
/// `FAST` dependent does: a task it queues is an enqueue by `main`'s thread,
/// which natively starts the idle worker.
#[test]
#[cfg_attr(miri, ignore)]
fn a_sync_dependent_on_mains_thread_wakes_the_idle_worker() {
    start_test(1);
    let p = promise_new().unwrap();
    let seen = Rc::new(Cell::new((false, false)));
    let s2 = seen.clone();
    let _d = depend(
        p,
        Box::new(move || {
            let in_sync = in_sync_task();
            let _x = spawn(Box::new(|| Outcome::Done), 0, true);
            s2.set((in_sync, with(|s| s.lone_worker_for_test().0)));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    resolve(p, || {});
    let (in_sync, woken) = seen.get();
    assert!(in_sync, "the dependent ran as a sync task");
    assert!(woken, "the enqueue did not wake the idle worker");
    finish();
}

/// Review RF15-A03: a dependent that `depend` runs at once on `main`
/// (`FAST`) is `main`'s own code for the lone worker's rules too. A task it
/// queues wakes the idle worker, as an enqueue by `main` does (`enqueue`);
/// a task its `wait` runs on `main`'s stack is the lone worker's, which
/// then picks the next task (`end`). Before the fix both rules read the raw
/// stack of running tasks, where the dependent counted as a task: the
/// worker was not woken, and it picked nothing after `x`. The woken
/// worker's wake-up is held off (`hold_wake_for_test`), so that it never
/// takes `x` itself: on the first, slow run of a fresh build its latency
/// (90 µs) passed before the `wait`, and the check passed without the fix
/// too.
#[test]
#[cfg_attr(miri, ignore)]
fn a_dependent_run_at_once_on_main_is_mains_code_for_the_lone_worker() {
    start_test(1);
    let seen = Rc::new(Cell::new((false, false)));
    let s2 = seen.clone();
    depend(
        TaskId::FINISHED,
        Box::new(move || {
            let x = spawn(Box::new(|| Outcome::Done), 0, true);
            let woken = with(|s| s.lone_worker_for_test().0);
            with(|s| s.hold_wake_for_test());
            let y = spawn(Box::new(|| Outcome::Done), 0, true);
            wait(x);
            let picked = with(|s| s.lone_worker_for_test().1 == Some(y));
            s2.set((woken, picked));
            Outcome::Done
        }),
        0,
        true,
        true,
    );
    let (woken, picked) = seen.get();
    assert!(woken, "the enqueue did not wake the idle worker");
    assert!(picked, "the worker that ran x did not pick y");
    finish();
}

/// Review AR-52 (fixes-14): with no event loop context alive, a timer that
/// comes due while the final run runs a task on `main`'s stack fires after
/// the task, as natively the loop thread fires it alongside the workers.
/// Before the fix the final run never looked at the timers then.
#[test]
#[cfg_attr(miri, ignore)]
fn the_final_run_fires_a_timer_that_came_due_during_a_task() {
    start_test(1);
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    timer_start(
        std::time::Instant::now() + std::time::Duration::from_millis(30),
        Rc::new(move || f2.set(true)),
    );
    let _t = spawn(
        Box::new(|| {
            std::thread::sleep(std::time::Duration::from_millis(80));
            Outcome::Done
        }),
        0,
        true,
    );
    finish();
    assert!(fired.get(), "the timer due during the task did not fire");
}

/// Review HU-01 (fixes-14): at an effect point, a timer due by now goes
/// first, as a due sleeper does: its callback runs before the effect point
/// returns. Before the fix the loop context started at the effect point
/// had been able to run for less than `STALE`, so the output came first.
#[test]
#[cfg_attr(miri, ignore)]
fn an_effect_point_lets_a_due_timer_go_first() {
    start_test(1);
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    timer_start(std::time::Instant::now(), Rc::new(move || f2.set(true)));
    std::thread::sleep(std::time::Duration::from_millis(1));
    effect();
    assert!(fired.get(), "the due timer did not go first");
    finish();
}

// ---------------------------------------------------------------------------
// fixes-16 (hunt HSK-01 to HSK-03): the stack room of a run on the waiter's
// stack, and the event loop's stack. The low end of `main`'s stack is set
// by hand (`ctx::TEST_THREAD_LOW`), so that `main`'s room is known without
// the feature `stack-overflow`; the contexts have 16 MiB, so a task needs
// 15 MiB of room to run on its waiter's stack (1 GiB, the native worker's
// size without `LEAN_STACK_SIZE_KB`, is more).

/// Review RF16-01: the slack is at most a sixteenth of the stack, so that a
/// small `LEAN_STACK_SIZE_KB` keeps the room rule: 1 MiB from 16 MiB up, 72
/// KiB for `LEAN_STACK_SIZE_KB=1024` (1152 KiB), never the whole stack.
#[test]
fn the_inline_slack_scales_with_a_small_stack() {
    assert_eq!(ctx::inline_slack(1 << 30), 1 << 20);
    assert_eq!(ctx::inline_slack(16 << 20), 1 << 20);
    assert_eq!(ctx::inline_slack(1152 << 10), 72 << 10);
    for kib in [64usize, 896, 1024, 1152, 4096] {
        let base = kib << 10;
        assert!(
            base - ctx::inline_slack(base) >= base / 16 * 15,
            "{kib} KiB"
        );
    }
}

/// The contexts' stack size of these tests.
const ROOM_CTX: usize = 16 << 20;

/// Start a scheduler with 16 MiB contexts and give `main` `room` bytes of
/// stack below the caller (`ctx::TEST_THREAD_LOW`); `false` if the room
/// rule cannot be checked here (`LEAN_STACK_SIZE_KB` set to less than the
/// contexts' size).
fn start_room_test(workers: u32, room: usize) -> bool {
    start_with(Rc::new(NoSuspend), workers, ROOM_CTX);
    let base = thread_stack_size().min(ROOM_CTX);
    let need = base - ctx::inline_slack(base);
    if need != ROOM_CTX - ctx::inline_slack(ROOM_CTX) {
        eprintln!("note: LEAN_STACK_SIZE_KB is set: the room rule is not checked");
        return false;
    }
    let here = 0u8;
    let sp = std::ptr::addr_of!(here).addr();
    ctx::TEST_THREAD_LOW.with(|c| c.set(sp - room));
    true
}

/// Where tasks ran: the context, and the usable size of its stack (`None`
/// on `main`'s).
type RanOn = Rc<RefCell<Vec<(CtxId, Option<usize>)>>>;

/// A job that records where it ran (`RanOn`).
fn ran_on_job(log: &RanOn) -> Job {
    let log = log.clone();
    Box::new(move || {
        let size = running_stack().map(|b| b.top - b.guard_hi);
        log.borrow_mut().push((current_context(), size));
        Outcome::Done
    })
}

/// With the room (64 MiB here), a needed task runs on its waiter's stack,
/// as before: 1000 `Task.spawn`/`Task.get` pairs on `main` start no
/// context (the cost of the rule in the common case is one look at the
/// stack pointer per wait).
#[test]
#[cfg_attr(miri, ignore)]
fn hsk01_with_room_a_needed_task_runs_on_the_waiters_stack() {
    if !start_room_test(2, 64 << 20) {
        return;
    }
    let log = Rc::new(RefCell::new(Vec::new()));
    for _ in 0..1000 {
        let t = spawn(ran_on_job(&log), 0, false);
        wait(t);
    }
    let t = spawn(ran_on_job(&log), 0, true);
    assert_eq!(wait_any(&[t]), 0);
    assert_eq!(log.borrow().len(), 1001);
    assert!(
        log.borrow().iter().all(|&(c, _)| c == MAIN),
        "a task ran on a context of its own"
    );
    need_ok();
    finish();
}

/// Without the room (2 MiB), a needed task that may run now starts on a
/// context of its own, with the contexts' stack, and the waiter waits for
/// it: an IO task, a pure one and a dedicated one through `Task.get`, and a
/// pool task through `IO.waitAny`. Before the fix each ran on `main`'s
/// stack (hunt HSK-01).
#[test]
#[cfg_attr(miri, ignore)]
fn hsk01_without_room_a_needed_task_runs_on_a_context_of_its_own() {
    if !start_room_test(2, 2 << 20) {
        return;
    }
    let log = Rc::new(RefCell::new(Vec::new()));
    let io = spawn(ran_on_job(&log), 0, true);
    wait(io);
    let pure = spawn(ran_on_job(&log), 0, false);
    wait(pure);
    let dedicated = spawn(ran_on_job(&log), 9, true);
    wait(dedicated);
    let any = spawn(ran_on_job(&log), 0, true);
    assert_eq!(wait_any(&[any]), 0);
    let got = log.borrow().clone();
    assert_eq!(got.len(), 4);
    for (k, &(c, size)) in got.iter().enumerate() {
        assert_ne!(c, MAIN, "task {k} ran on main's stack");
        assert_eq!(size, Some(ROOM_CTX), "task {k}: its context's stack");
    }
    assert!([io, pure, dedicated, any].iter().all(|&t| is_finished(t)));
    need_ok();
    finish();
}

/// Without the room, a pure task the woken worker starts in the waiter's
/// own look (`PICKED`) starts on a context of its own, and the waiter
/// blocks on it: the started task's waiter is no hang there (a debug build
/// checks it in `register_block`). A watched descriptor keeps the hub from
/// its last resort, and a timer ends the watch after 2 s, so a waiter
/// blocked with nothing to start the task fails the test.
#[test]
#[cfg_attr(miri, ignore)]
fn hsk01_without_room_a_started_pure_task_runs_on_a_context_of_its_own() {
    if !start_room_test(1, 2 << 20) {
        return;
    }
    let (r, w) = rustix::pipe::pipe().unwrap();
    let wid = watch(r, Interest::READ, Rc::new(|_| {})).unwrap();
    let fired = Rc::new(Cell::new(false));
    let f2 = fired.clone();
    let timer = timer_start(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
        Rc::new(move || {
            f2.set(true);
            unwatch(wid);
        }),
    );
    let log = Rc::new(RefCell::new(Vec::new()));
    let p = spawn(ran_on_job(&log), 0, false);
    // the worker's latency passes: it starts `p` in `main`'s look
    std::thread::sleep(std::time::Duration::from_millis(2));
    wait(p);
    assert!(!fired.get(), "main waited until the timer ended the watch");
    let got = log.borrow().clone();
    assert_eq!(got.len(), 1);
    assert_ne!(got[0].0, MAIN, "p ran on main's stack");
    need_ok();
    assert!(timer_stop(timer));
    unwatch(wid);
    drop(w);
    finish();
}

/// The final run (`finish`) without the room on `main`'s stack runs each
/// remaining task on a context of its own, in the order it would run them
/// on `main`'s (hunt HSK-02: `main` on the process's thread has 8 MiB
/// natively, the workers 1 GiB). With the room, on `main`'s stack, as
/// before.
#[test]
#[cfg_attr(miri, ignore)]
fn hsk02_the_final_run_without_room_runs_tasks_on_contexts() {
    for (room, on_main) in [(2 << 20, false), (64 << 20, true)] {
        std::thread::spawn(move || {
            if !start_room_test(1, room) {
                return;
            }
            let l = log();
            let order = Rc::new(RefCell::new(Vec::new()));
            for name in ["a", "b", "c"] {
                let (l, order) = (l.clone(), order.clone());
                spawn(
                    Box::new(move || {
                        l.borrow_mut().push(name.to_string());
                        order.borrow_mut().push(current_context() == MAIN);
                        Outcome::Done
                    }),
                    0,
                    true,
                );
            }
            finish();
            assert_eq!(entries(&l), ["a", "b", "c"], "room {room}");
            assert!(
                order.borrow().iter().all(|&m| m == on_main),
                "room {room}: on main {:?}",
                order.borrow()
            );
        })
        .join()
        .unwrap();
    }
}

/// The event loop's context has a native loop thread's stack, 1 GiB,
/// whatever the contexts' size (natively libuv's loop thread is made
/// before `LEAN_STACK_SIZE_KB` is read; hunt HSK-03), and its stack is
/// kept for the next loop context; a worker context keeps the contexts'
/// size.
#[test]
#[cfg_attr(miri, ignore)]
fn hsk03_the_loop_context_has_a_native_loop_threads_stack() {
    start_test(2);
    let seen: Rc<RefCell<Vec<StackBounds>>> = Rc::new(RefCell::new(Vec::new()));
    for ms in [5, 30] {
        let (p, cb, _) = waitable();
        let s2 = seen.clone();
        let cb: Rc<dyn Fn()> = Rc::new(move || {
            s2.borrow_mut().push(running_stack().expect("on a context"));
            cb();
        });
        timer_start(
            std::time::Instant::now() + std::time::Duration::from_millis(ms),
            cb,
        );
        wait(p);
    }
    let seen = seen.borrow().clone();
    assert_eq!(seen.len(), 2);
    for b in &seen {
        assert_eq!(b.top - b.guard_hi, 1 << 30, "the loop context's stack");
    }
    assert_eq!(seen[0], seen[1], "the second loop context reused the stack");
    let log = Rc::new(RefCell::new(Vec::new()));
    let p = promise_new().unwrap();
    spawn(ran_on_job(&log), 0, true);
    // `main` blocks: the task starts on a worker context
    let t0 = std::time::Instant::now();
    timer_start(t0 + std::time::Duration::from_millis(20), {
        Rc::new(move || {
            resolve(p, || {});
        })
    });
    wait(p);
    assert_eq!(log.borrow()[0].1, Some(1 << 20), "a worker context's stack");
    finish();
}
