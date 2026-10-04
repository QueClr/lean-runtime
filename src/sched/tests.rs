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

#[test]
fn tasks_are_deferred_until_needed() {
    start_test(4);
    let l = log();
    let a = spawn(job(&l, "a"), 0, true);
    let b = spawn(job(&l, "b"), 0, false);
    assert!(entries(&l).is_empty());
    assert!(!is_finished(a));
    wait(b);
    assert_eq!(entries(&l), ["b"]);
    wait(a);
    wait(a);
    assert_eq!(entries(&l), ["b", "a"]);
    assert!(is_finished(a) && is_finished(b));
    finish();
    assert_eq!(entries(&l), ["b", "a"]);
}

#[test]
fn the_final_run_goes_by_priority() {
    start_test(1);
    let l = log();
    // Created inside a task (natively on a worker thread), so that no idle
    // worker wakes for them: Lean's queues alone decide the order.
    let l2 = l.clone();
    spawn(
        Box::new(move || {
            spawn(job(&l2, "default 1"), 0, true);
            spawn(job(&l2, "max"), 8, true);
            spawn(job(&l2, "prio 3"), 3, true);
            spawn(job(&l2, "default 2"), 0, true);
            spawn(job(&l2, "dedicated"), 9, true);
            // 2^32 + 4 is priority 4 (an `unsigned` in Lean's runtime).
            spawn(job(&l2, "prio 2^32+4"), (1 << 32) + 4, true);
            Outcome::Done
        }),
        u32::MAX as u64,
        true,
    );
    assert!(entries(&l).is_empty());
    finish();
    assert_eq!(
        entries(&l),
        [
            "dedicated",
            "max",
            "prio 2^32+4",
            "prio 3",
            "default 1",
            "default 2"
        ]
    );
}

#[test]
fn sync_priority_runs_at_once_on_the_current_thread() {
    start_test(4);
    let th = Rc::new(Cell::new(u64::MAX));
    let th2 = th.clone();
    let id = spawn(
        Box::new(move || {
            th2.set(thread_number());
            Outcome::Done
        }),
        u32::MAX as u64,
        true,
    );
    assert!(is_finished(id));
    assert_eq!(th.get(), 0, "LEAN_SYNC_PRIO runs on main's thread");
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

/// Run `f` as a task on `main`'s thread (priority `LEAN_SYNC_PRIO`): the
/// tasks it creates wake no idle worker (natively it runs on a worker
/// thread), so the lone worker's pick (`settle_worker`, by elapsed time)
/// stays out of the test.
fn in_task(f: impl FnOnce() + 'static) {
    spawn(
        Box::new(move || {
            f();
            Outcome::Done
        }),
        u32::MAX as u64,
        true,
    );
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
    sleep_ms(50);
    assert_eq!(entries(&l), ["pure", "io dependent"]);
    finish();
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
    sleep_ms(50);
    assert_eq!(entries(&l), ["t", "u", "v", "io"]);
    finish();
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
    wait(b);
    assert_eq!(entries(&l), ["f", "inner", "copy inner's value"]);
}

#[test]
fn wait_any_takes_a_finished_task_else_runs_a_pending_one() {
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
    sleep_ms(5);
    assert_eq!(entries(&l), ["io"]);
    assert!(is_finished(id));
    finish();
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

/// A glue whose `idle` hook blocks, which the hub forbids (it runs on
/// `main`'s stack; docs/sched.md, S4).
struct BlockingIdle;

impl Glue for BlockingIdle {
    fn suspend(&self, _: Suspend<'_>) {
        panic!("the crate's unit tests never suspend a context");
    }
    fn idle(&self, _: Option<std::time::Instant>) {
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
        start_with(Rc::new(BlockingIdle), 1, 1 << 20);
        // `main` waits for a promise no one resolves: the hub has nothing to
        // run and no sleeper, so it waits in `idle`, which tries to block.
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
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sleep_ms(5)));
    assert!(r.is_err(), "the context's panic goes on in main");
    assert_eq!(current_context(), MAIN);
    with(|s| {
        assert_eq!(s.cx.ctxs[MAIN].status, ctx::Status::Running);
        assert_eq!(s.cx.blocked, 0);
        assert_eq!(s.cx.workers, 0);
    });
    let l = log();
    spawn(job(&l, "after"), 0, true);
    sleep_ms(5);
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
    sleep_ms(50);
    need_ok();
    assert_eq!(entries(&l), ["t", "b", "w", "b2", "io"]);
    finish();
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
    sleep_ms(30); // x0 and d run on a worker context; d now waits for s
    need_ok();
    wait(io);
    need_ok();
    finish();
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
            sleep_ms(5);
            Outcome::Done
        }),
        0,
        true,
    );
    spawn(Box::new(|| panic!("boom in a context")), 0, true);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait(t1)));
    assert!(r.is_err());
    assert_eq!(thread_number(), 0, "main's bookkeeping still holds t1");
    assert_eq!(with(|s| s.cx.in_use), 0, "a worker still counted in use");
    assert!(!is_finished(t1), "the abandoned task stays unfinished");
    need_ok();
    // `main` goes on.
    let l = log();
    spawn(job(&l, "after"), 0, true);
    sleep_ms(5);
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
