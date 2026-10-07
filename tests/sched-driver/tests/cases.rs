//! Runs the Rust port (`sched-cases ID`) of every case of
//! `tests/cases/{tasks,sync,refs,taskio,uvloop,net}`, and of the cases of
//! `tests/cases/io` and `tests/cases/process` that create tasks, over the
//! single-thread scheduler, once each, as `scripts/cases.py check` runs a
//! translator's executable (`runner`), and compares its stdout, stderr and
//! exit code with the case's expected ones: native's, or the correct ones
//! where native has a Lean bug (LB-01, LB-13), or a recorded alternative of
//! a schedule-dependent case or of a known difference (LSCHED-xx).
//! `tests/sched-driver-mt` runs the task cases in threads mode.

mod runner;
use runner::*;
use std::time::{Duration, Instant};

/// The driver's binary.
const EXE: &str = env!("CARGO_BIN_EXE_sched-cases");

/// The areas whose cases all run through `sched`.
const AREAS: &[&str] = &["tasks", "sync", "refs", "taskio", "uvloop", "net"];

/// The cases of other areas that create tasks, which run through `sched`
/// too, with their areas.
const WITH_TASKS: &[(&str, &str)] = &[
    ("io", "lock_blocked"),
    ("io", "lock_exit"),
    ("io", "lock_during_read"),
    ("process", "exit_while_reading"),
    ("process", "exit_while_writing"),
    ("process", "exit_while_writing_stalled"),
    ("process", "handoff_then_resolve"),
    ("process", "handoff_then_write"),
    ("process", "handoff_then_kill"),
];

fn check(id: &str) {
    check_env(id, &[]);
}

fn check_env(id: &str, extra: &[(String, String)]) {
    let exp = expected(id);
    assert!(!exp.is_empty(), "{id}: no recorded outcome");
    let got = run_env(id, extra);
    let ok = exp
        .iter()
        .any(|e| e.out == got.out && e.err == got.err && e.code == got.code);
    assert!(
        ok,
        "{id}: got code {} stdout {:?} stderr {:?}; expected code {} stdout {:?} stderr {:?}",
        got.code,
        String::from_utf8_lossy(&got.out),
        String::from_utf8_lossy(&got.err),
        exp[0].code,
        String::from_utf8_lossy(&exp[0].out),
        String::from_utf8_lossy(&exp[0].err),
    );
}

/// An alternative that a case gives one translator (leanrs's DV11 order of
/// `tasks/promise_nested_free_order`, `alternatives = { alt1 = "leanrs" }`)
/// is not an outcome the crate's ports may have; one of a case with no
/// `alternatives` is (`runaway_pure_task_before_io.alt1`, LSCHED-01).
#[test]
fn owned_alternative_not_accepted() {
    let id = "promise_nested_free_order";
    assert!(case_dir(id).join(format!("{id}.alt1.code")).exists());
    assert_eq!(
        alternatives(id),
        Some(vec![("alt1".to_string(), "leanrs".to_string())])
    );
    assert_eq!(expected(id).len(), 1);
    assert_eq!(alternatives("runaway_pure_task_before_io"), None);
    assert_eq!(expected("runaway_pure_task_before_io").len(), 2);
}

/// The glue's lazy start (`sched::start_lazy`, lean2rr's; review RSH2-09):
/// cases whose first scheduler use is each kind of entry point (a task, a
/// promise, `Std.Sync` objects, a timer, a signal watcher, the loop's
/// configuration, `ST.Ref` reads polled by `main`, a TCP and a UDP socket,
/// a DNS lookup) give their recorded outcomes with the scheduler built
/// there, not at `main`'s start. The glue asserts that nothing is built at
/// `start_lazy` and that the scheduler was built by the end of `main`, so
/// the test fails if the lazy path is not taken (review RSH2-12: checked
/// with `start_lazy` mutated into `start_with`).
#[test]
fn lazy_start_cases() {
    let lazy = [("SCHED_DRIVER_LAZY".to_string(), "1".to_string())];
    for id in [
        "promise_across_tasks",
        "dropped_pure_task",
        "mutex_handoff",
        "recursive_mutex",
        "condvar_turns",
        "timer_oneshot",
        "signal_usr1",
        "loop_configure",
        "lost_update",
        "tcp_echo",
        "udp_basic",
        "dns_localhost",
    ] {
        check_env(id, &lazy);
    }
}

/// Every case of the areas has a port (no case is left out silently).
#[test]
fn every_case_is_ported() {
    for a in AREAS {
        for e in std::fs::read_dir(cases_root().join(a)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "lean") {
                let id = p.file_stem().unwrap().to_string_lossy().into_owned();
                assert!(
                    PORTED.contains(&id.as_str()),
                    "tests/cases/{a}/{id}.lean has no port in sched-driver"
                );
            }
        }
    }
}

/// A Rust panic (not a Lean panic: a bug of a translator or the runtime) in
/// a task on a context of its own unwinds to the context's base, and is
/// resumed on `main`'s stack, where it goes on as a panic of `main`
/// (docs/sched.md, "Rust panics"): Rust's message, status 101.
#[test]
fn a_rust_panic_in_a_context_goes_on_in_main() {
    let got = run_with("rust_panic_in_task", &[], &[], None, false);
    let err = String::from_utf8_lossy(&got.err);
    assert_eq!(got.code, "101", "stderr {err:?}");
    assert!(err.contains("a Rust panic in a task"), "stderr {err:?}");
    assert!(!err.contains("not reached"));
}

/// Review LF3-01, the effect points' half: a started pure task that runs
/// for a waiter and reaches effect points lets the next started task run,
/// whose worker takes the awaited task (two workers); the runaway then
/// keeps the exit waiting.
#[test]
fn effect_points_in_a_started_task_let_the_next_run() {
    let env = [("LEAN_NUM_THREADS".to_string(), "2".to_string())];
    let got = run_with("effect_points_in_a_started_task", &[], &env, Some(3), false);
    let err = String::from_utf8_lossy(&got.err);
    assert_eq!(got.code, "timeout", "stderr {err:?}");
    assert_eq!(err, "t = 1001\np finished: false, q finished: true\n");
}

/// The same panic, resumed while `main` is in an effect point: the hooks'
/// cold paths cannot unwind (review AR-28; docs/sched.md, "Rust panics"),
/// so the process aborts there, as at an FFI boundary: status 134.
#[test]
fn a_rust_panic_resumed_in_a_hooks_cold_path_aborts() {
    let got = run_with("rust_panic_through_effect", &[], &[], None, false);
    let err = String::from_utf8_lossy(&got.err);
    assert_eq!(got.code, "134", "stderr {err:?}");
    assert!(err.contains("a Rust panic in a task"), "stderr {err:?}");
    assert!(err.contains("cannot unwind"), "stderr {err:?}");
    assert!(!err.contains("not reached"));
}

// leanrs's adversarial checks of docs/sched.md's "Why Glue::suspend is
// sound" (their proof check of sched-1 at 15b62ed, 2026-10-03), ported as
// regression tests. The programs are in `src/cases.rs`.

fn err_of(o: &Outcome) -> String {
    String::from_utf8_lossy(&o.err).into_owned()
}

/// S6: a destructor run while a context unwinds blocks there (a promise's
/// `sync` dependent waits for a sleeping task): the context suspends
/// mid-unwind, resumes, and the panic goes on in `main`.
#[test]
fn adv_block_in_drop_during_unwind() {
    let got = run_with("adv_block_in_drop_during_unwind", &[], &[], None, false);
    let err = err_of(&got);
    assert_eq!(got.code, "101", "stderr {err:?}");
    assert!(err.contains("boom in task"), "stderr {err:?}");
    assert!(
        err.contains("sync dep runs in drop during unwind"),
        "stderr {err:?}"
    );
    assert!(
        err.contains("sync dep got 7 after block, v = None"),
        "stderr {err:?}"
    );
    assert!(!err.contains("not reached"), "stderr {err:?}");
}

/// S6: a second panic in a destructor during unwinding aborts, as in plain
/// Rust.
#[test]
fn adv_panic_in_sync_dep_of_drop() {
    let got = run_with("adv_panic_in_sync_dep_of_drop", &[], &[], None, false);
    let err = err_of(&got);
    assert_eq!(got.code, "134", "stderr {err:?}");
    assert!(err.contains("second panic in sync dep"), "stderr {err:?}");
    assert!(
        err.contains("panic in a destructor during cleanup"),
        "stderr {err:?}"
    );
    assert!(!err.contains("not reached"), "stderr {err:?}");
}

/// S5: `process::exit` from a task's context runs the thread's TLS
/// destructors on the coroutine's stack; the scheduler's only forget.
#[test]
fn adv_exit_from_task() {
    let got = run_with("adv_exit_from_task", &[], &[], None, false);
    assert_eq!(got.code, "3", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "task exits\n");
    assert!(got.out.is_empty());
}

/// AR-39 (lean2rr's review RS7-02): an initializer keeps a recursive mutex
/// locked and `main`, on the same OS thread (this glue's, as
/// `LEAN_MAIN_USE_THREAD=0` natively), locks it again: with workers (the
/// eager start and the lazy one) a dedicated task cannot take it, and can
/// once `main` has unlocked it three times; with `LEAN_NUM_THREADS=0` the
/// tasks run at once on `main`'s thread, so the first takes it too. Before
/// AR-39 the owner held whether the scheduler had started, and with workers
/// `main`'s `tryLock` was false and its `lock` waited for good.
#[test]
fn rs7_init_reclock() {
    let env = |kv: &[(&str, &str)]| -> Vec<(String, String)> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let with_workers = "main tryLock: true\nmain has the lock\ntask tryLock: false\n\
                        task tryLock after the unlocks: true\n";
    let no_workers = "main tryLock: true\nmain has the lock\ntask tryLock: true\n\
                      task tryLock after the unlocks: true\n";
    for (kv, want) in [
        (&[][..], with_workers),
        (&[("SCHED_DRIVER_LAZY", "1")][..], with_workers),
        (&[("LEAN_NUM_THREADS", "0")][..], no_workers),
        (
            &[("LEAN_NUM_THREADS", "0"), ("SCHED_DRIVER_LAZY", "1")][..],
            no_workers,
        ),
    ] {
        let got = run_with("rs7_init_reclock", &[], &env(kv), Some(10), false);
        assert_eq!(got.code, "0", "{kv:?}: stderr {:?}", err_of(&got));
        assert_eq!(out_of(&got), want, "{kv:?}");
        assert!(got.err.is_empty(), "{kv:?}: stderr {:?}", err_of(&got));
    }
}

/// RNET-01 of net-1's review: a receive's allocation that ends the process
/// (Lean's internal panic, an effect point) while a task uses the same
/// socket ends it as native does, not with a Rust panic.
#[test]
fn rnet_alloc_reentry() {
    for kind in ["tcp", "udp"] {
        let got = run_with("rnet_alloc_reentry", &[kind.to_string()], &[], None, false);
        assert_eq!(got.code, "1", "{kind}: stderr {:?}", err_of(&got));
        assert_eq!(
            err_of(&got),
            "INTERNAL PANIC: integer overflow in runtime computation\n",
            "{kind}"
        );
        assert_eq!(
            String::from_utf8_lossy(&got.out),
            "calling recv with a huge size\n",
            "{kind}"
        );
    }
}

/// RNET-02 of net-1's review (LB-28): a shutdown requested while the
/// connect is surely pending happens once the connect succeeds, and fails
/// with `ECANCELED` behind a connect that fails.
#[test]
fn rnet_shutdown_in_connect() {
    let got = run_with("rnet_shutdown_in_connect", &[], &[], None, false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(
        String::from_utf8_lossy(&got.out),
        "connect: ok\nshutdown: ok\npeer recv?: ok none (end of stream)\n\
         refused connect: error no such thing (error code: 111, connection refused)\n\
         its shutdown: error operation canceled (error code: 125)\n"
    );
}

// The regression programs of sched-io's reviews (src/review.rs).

fn out_of(o: &Outcome) -> String {
    String::from_utf8_lossy(&o.out).into_owned()
}

/// RSIO-01: a stream lock held across `wait_fd` is waited for by `main`.
#[test]
fn rsio_poll_fds_with_stream_lock() {
    let got = run_with("rsio_poll_fds_with_stream_lock", &[], &[], Some(10), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "main: printed\n");
    assert_eq!(
        err_of(&got),
        "main: printing\ntask: waited holding stdout\nmain: done\n"
    );
}

/// RSIO-02: a blocking watch callback neither makes the hub spin nor is
/// followed by a call on its drained descriptor.
#[test]
fn rsio_watch_spin() {
    let got = run_with("rsio_watch_spin", &[], &[], Some(10), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(
        out_of(&got),
        "callback calls: 1\nunder 100 ms of CPU while the callback slept: true\n"
    );
}

/// RSIO-03 and RSIO-09: no other context runs during a handle's drop in a
/// no-suspend scope (its flush waits for the context's next scheduling
/// point, AR-8); without the scope, other contexts run during the drop. The
/// child gets every byte.
#[test]
fn rsio_drop_no_suspend() {
    let got = run_with("rsio_drop_no_suspend", &[], &[], Some(10), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "65636\nticks during the drop: 0\n");
    let got = run_with(
        "rsio_drop_no_suspend",
        &["plain".to_string()],
        &[],
        Some(10),
        false,
    );
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "65636\nticks during the drop: 1\n");
}

/// RSIO-09 (round 2): a handle dropped unflushed in a no-suspend scope,
/// its pipe full until a task of this program drains the child: the flush
/// waits for `main`'s next scheduling point, then completes (natively it
/// completes too).
#[test]
fn rsio_ns_drop_deadlock() {
    for a in [&[][..], &["plain".to_string()][..]] {
        let got = run_with("rsio_ns_drop_deadlock", a, &[], Some(20), false);
        assert_eq!(got.code, "0", "{a:?}: stderr {:?}", err_of(&got));
        assert_eq!(
            err_of(&got),
            "main: wrote, dropping stdin\nmain: dropped\n",
            "{a:?}"
        );
        assert_eq!(out_of(&got), "read 200000\nexit Some(0)\n", "{a:?}");
    }
}

/// RSIO-09 (round 2): taskio/task_reads_main_writes without its final
/// flush, the handle dropped in a no-suspend scope (it hung 1 run in 15).
#[test]
fn rsio_ns_cat() {
    for _ in 0..3 {
        let args = ["3000".to_string(), "1000".to_string()];
        let got = run_with("rsio_ns_cat", &args, &[], Some(20), false);
        assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
        assert_eq!(
            out_of(&got),
            "main: wrote everything\ntask: read Some(3003000)\ncat exited Some(0)\n"
        );
    }
}

/// RSIO-10 (round 2): a sync dependent of a promise dropped in a drain
/// (run after it, since wait-1) sleeps; `scope_wait`: `main` sleeps inside
/// its own no-suspend scope (review RW1-04); the other contexts do not
/// inherit the scope.
#[test]
fn rsio_ns_leak() {
    for a in [
        &[][..],
        &["plain".to_string()][..],
        &["scope_wait".to_string()][..],
    ] {
        let got = run_with("rsio_ns_leak", a, &[], Some(10), false);
        assert_eq!(got.code, "0", "{a:?}: stderr {:?}", err_of(&got));
        assert_eq!(err_of(&got), "reader: io_cooperative = true\n", "{a:?}");
        assert_eq!(out_of(&got), "reader got \"hi\\n\"\n", "{a:?}");
    }
}

/// RSIO-10 (round 2): a task sleeping in a no-suspend scope does not make
/// `main` (in no scope) panic on a stream another task holds.
#[test]
fn rsio_ns_leak_panic() {
    let got = run_with("rsio_ns_leak_panic", &[], &[], Some(10), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "main: printing (no scope here)\nmain: done\n");
    assert_eq!(out_of(&got), "main: printed\n");
}

/// AR-8: a drop walk ending inside a panic's unwinding with a stream set
/// aside: the leave does not suspend (no tick, the panic in flight), and the
/// child gets every byte at `main`'s next scheduling point, or once `main`
/// has returned (`exit`).
#[test]
fn rsio_ns_unwind() {
    for (a, out) in [
        (vec![], "65636\nmain: after the unwind\n"),
        (vec!["exit".to_string()], "65636\n"),
    ] {
        let got = run_with("rsio_ns_unwind", &a, &[], Some(10), false);
        assert_eq!(got.code, "0", "{a:?}: stderr {:?}", err_of(&got));
        assert_eq!(
            err_of(&got),
            "drop walk: panicking true, ticks during the leave 0\n",
            "{a:?}"
        );
        assert_eq!(out_of(&got), out, "{a:?}");
    }
}

/// AR-8 (leanrs's review of fixes-1): `force_exit` right after a drop that
/// set a stream aside writes the stream's bytes first (natively the drop's
/// `fclose` wrote them), and nothing else: the child gets 65636 bytes, and
/// `main`'s buffered line is lost.
#[test]
fn rsio_ns_force_exit() {
    let got = run_with("rsio_ns_force_exit", &[], &[], Some(10), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "65636\n");
}

/// RFX1-01 and RFX1-02 (review of fixes-1): a stream handed off by a drop in
/// a no-suspend scope, then `main` reads a handle a task drains (waiting for
/// its lock, or taking it between the task's reads), with the scope and
/// without: every mode ends, as natively (`SharedRead.lean`).
#[test]
fn rfx1_shared_read() {
    for mode in ["", "sleepy", "plain", "sleepy-plain"] {
        let got = run_with(
            "rfx1_shared_read",
            &[mode.to_string()],
            &[],
            Some(20),
            false,
        );
        assert_eq!(got.code, "0", "{mode:?}: stderr {:?}", err_of(&got));
        assert_eq!(
            out_of(&got),
            "main read Ok(true); reader read some: true\nexit Some(0)\n",
            "{mode:?}"
        );
        assert_eq!(
            err_of(&got),
            "main: dropped, reading\nmain: read Ok(true)\n",
            "{mode:?}"
        );
    }
}

/// RFX1-03: after the drop, `main` only polls `Child.tryWait`; the handed-off
/// bytes reach `wc -c` anyway, and the loop ends (`TryWait.lean`).
#[test]
fn rfx1_trywait() {
    let got = run_with("rfx1_trywait", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "65636\nchild exited 0 (spun: true)\n");
}

/// LB-29's scope (the judge's ruling): `IO.Process.exit` (or, with `force`,
/// `forceExit`) right after a drop handed a stream to a writer thread, whose
/// pipe's reader (`wc -c`) reads only once a task of this program has
/// drained its 300000-byte standard output. The exit's join of the writer
/// lets that task run (cooperatively, as natively the other threads run
/// during the drop's `fclose` and the exit): the exit completes and `wc`
/// gets every byte.
#[test]
fn rsio_exit_join() {
    for (a, out) in [("exit", "main: exiting\n"), ("force", "")] {
        let got = run_with("rsio_exit_join", &[a.to_string()], &[], Some(20), false);
        assert_eq!(got.code, "0", "{a}: stderr {:?}", err_of(&got));
        assert_eq!(out_of(&got), out, "{a}");
        assert_eq!(err_of(&got).trim(), "65636", "{a}");
    }
}

/// RFX1-07 (round 2 of the fixes-1 review): `main` hands off a stream whose
/// reader reads only after a task has drained the child's 600000-byte
/// standard output (with a 0.5 s pause), then ends: returns (`finish` waits
/// for `main`'s writer), `IO.Process.exit 0` or `forceExit 0` (both wait for
/// the exiting context's writer, letting the task run). Natively the drop's
/// `fclose` waits, and each ends after about 0.5 s; here each ends.
#[test]
fn rfx2_exit_handoff() {
    for mode in ["return", "exit", "force"] {
        let got = run_with(
            "rfx2_exit_handoff",
            &[mode.to_string()],
            &[],
            Some(20),
            false,
        );
        assert_eq!(got.code, "0", "{mode}: stderr {:?}", err_of(&got));
        assert_eq!(err_of(&got), format!("main: dropped, ending ({mode})\n"));
    }
}

/// RFX1-09: a task hands off a stream to `sleep 30`, which never reads; its
/// job waits for its writer (as natively its thread is in `fclose`). `main`
/// exits 3 after 300 ms without waiting for that writer, as native's `exit`
/// does not wait for another thread's `fclose` in progress (native: 0.31 s).
#[test]
fn rfx2_exit_unrelated_handoff() {
    let t0 = Instant::now();
    let got = run_with("rfx2_exit_unrelated_handoff", &[], &[], Some(10), false);
    assert_eq!(got.code, "3", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "main: exiting\n");
    // waiting for the writer would take the 30 s of `sleep`
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}

/// RFX1-07, the causal case: a task hands off a stream to `sleep 0.3; wc -c`
/// and ends; `main` waits for the task, then exits at once. The task's job
/// ended only when its writer had (natively its `fclose` returned first), so
/// `wc` gets every byte.
#[test]
fn rfx2_causal_handoff() {
    let got = run_with("rfx2_causal_handoff", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got).trim(), "65636");
}

/// RFX1-08 under LB-29's narrowed rule: a task is suspended writing a
/// 200001-byte line to standard output, a pipe whose reader starts after
/// 1 s, when `main` calls `IO.Process.exit 0`: the exit waits for the
/// writer (cooperatively: the task finishes its write), then flushes, and
/// the reader counts every byte, as natively.
#[test]
fn rfx2_exit_writer_held() {
    let got = run_line("rfx2_exit_writer_held", "$BIN | (sleep 1; wc -c)", Some(20));
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got).trim(), "200001");
}

/// RFX1-14 (round 3; leanrs's causal gap): a task hands off a stream, then
/// resolves a promise and goes on; `main` waits for the promise, then exits
/// or returns. The resolution waits for the task's writer (as natively its
/// `fclose` had returned), so `wc` counts 65636 either way
/// (`PromiseHandoff.lean`).
#[test]
fn rfx3_promise_handoff() {
    for mode in ["exit", "return"] {
        let got = run_with(
            "rfx3_promise_handoff",
            &[mode.to_string()],
            &[],
            Some(20),
            false,
        );
        assert_eq!(got.code, "0", "{mode}: stderr {:?}", err_of(&got));
        assert_eq!(out_of(&got).trim(), "65636", "{mode}");
    }
}

/// RFX1-17 (round 3): `IO.Process.exit` from an event-loop callback while a
/// task is suspended writing 200001 bytes to standard output (`callback`),
/// or two contexts exiting while it does (`two`: 4 or 5): the exit waits for
/// the writer, and every byte is delivered, as natively.
#[test]
fn rfx3_exit_contexts() {
    for (mode, codes) in [("callback", &["0"][..]), ("two", &["4", "5"][..])] {
        let got = run_line(
            "rfx3_exit_contexts",
            &format!("$BIN {mode} | (sleep 1; wc -c)"),
            Some(20),
        );
        assert!(
            codes.contains(&got.code.as_str()),
            "{mode}: code {} stderr {:?}",
            got.code,
            err_of(&got)
        );
        assert_eq!(out_of(&got).trim(), "200001", "{mode}");
    }
}

/// RFX1-18 (round 4; leanrs's io-entry gap): a task hands off, then tells
/// `main` through the file system (`createDir`); `main` polls for the
/// directory, then exits. The directory's creation waits for the task's
/// writer, so `wc` counts 65636, as native's `FsSignal.lean`.
#[test]
fn rfx4_fs_signal() {
    let got = run_with("rfx4_fs_signal", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got).trim(), "65636");
}

/// RFX1-19: a hand-off leaves no descriptor open once its writer has ended
/// (natively the drop closed the pipe): as many descriptors after the
/// writer, and after a scheduling point, as before the spawn.
#[test]
fn rfx4_fd_after() {
    let got = run_with("rfx4_fd_after", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    let out = out_of(&got);
    let n: Vec<&str> = out
        .trim()
        .trim_start_matches("descriptors: ")
        .split(", ")
        .map(|p| p.rsplit(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(n.len(), 3, "{out:?}");
    assert!(n[0] == n[1] && n[1] == n[2], "{out:?}");
}

/// RSIO-12 and RSIO-13 (round 3): a handle dropped with bytes the full pipe
/// does not take: the child gets every byte, and the modelled errno is as
/// before the drop (native's blocking `fclose` sets none), in a no-suspend
/// scope or not, the pipe filled to a page boundary or not.
#[test]
fn rsio_ns_partial() {
    for args in [
        vec![],
        vec!["plain"],
        vec!["scope", "full"],
        vec!["plain", "full"],
    ] {
        let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let fill = if args.get(1) == Some(&"full") {
            65536
        } else {
            65440
        };
        let got = run_with("rsio_ns_partial", &a, &[], Some(20), false);
        assert_eq!(got.code, "0", "{args:?}: stderr {:?}", err_of(&got));
        let out = out_of(&got);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{args:?}: {out:?}");
        assert_eq!(
            lines[0].trim(),
            (fill + 500).to_string(),
            "{args:?}: {out:?}"
        );
        let e: Vec<&str> = lines[1].rsplit(' ').collect();
        // "...; errno before B after A"
        assert_eq!(e[0], e[2], "{args:?}: {out:?}");
        assert!(lines[1].starts_with(&format!("expected {} bytes", fill + 500)));
    }
}

/// Lean's message (`src/runtime/stack_overflow.cpp`).
const STACK_OVERFLOW: &str = "\nStack overflow detected. Aborting.\n";

/// A small stack for the contexts (`LEAN_STACK_SIZE_KB`), as the case
/// `tasks/stack_overflow_in_task` sets it.
fn small_stacks() -> Vec<(String, String)> {
    vec![("LEAN_STACK_SIZE_KB".into(), "1024".into())]
}

/// AR-11: `main`'s own stack overflows after a task ran on a context: the
/// crate's handler knows the guard of the thread `glue::run` registered.
#[test]
fn so_main_overflow() {
    let got = run_with(
        "so_main_overflow",
        &["100000000".into()],
        &small_stacks(),
        None,
        false,
    );
    assert_eq!(got.code, "134", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), format!("task ran\n{STACK_OVERFLOW}"));
    assert!(got.out.is_empty(), "{:?}", out_of(&got));
}

/// AR-11: a task overflows its context's stack after switches.
#[test]
fn so_task_overflow_after_switches() {
    let got = run_with(
        "so_task_overflow_after_switches",
        &["100000000".into()],
        &small_stacks(),
        None,
        false,
    );
    assert_eq!(got.code, "134", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), format!("b ran\n{STACK_OVERFLOW}"));
    assert!(got.out.is_empty(), "{:?}", out_of(&got));
}

/// AR-11: a fault that is no overflow takes the default action, without a
/// message.
#[test]
fn so_segv_in_task() {
    let got = run_with("so_segv_in_task", &[], &small_stacks(), None, false);
    assert_eq!(got.code, "139", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "");
}

/// Review SO-1 of AR-11: a fault that is no overflow goes to a one-shot
/// previous handler (`SA_RESETHAND`) as the kernel would call it, with the
/// default restored first: one `prev`, then the default action (139), as
/// without the crate. Calling it with the crate's handler still installed
/// loops forever.
#[test]
fn so_prev_resethand() {
    let got = run_with("so_prev_resethand", &[], &[], Some(10), false);
    assert_eq!(got.code, "139", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "faulting\nprev\n");
}

/// AR-11: the fault of a thread that never registered goes on to Rust's
/// handler, which reports its overflow.
#[test]
fn so_rust_thread_overflow() {
    let got = run_with(
        "so_rust_thread_overflow",
        &["100000000".into()],
        &[],
        None,
        false,
    );
    let err = err_of(&got);
    assert_eq!(got.code, "134", "stderr {err:?}");
    assert!(err.contains("thread 'plain'"), "stderr {err:?}");
    assert!(err.contains("has overflowed its stack"), "stderr {err:?}");
    assert!(!err.contains("Stack overflow detected"), "stderr {err:?}");
}

/// AR-11: `sched::start` on another thread registers it.
#[test]
fn so_second_scheduler_thread() {
    let got = run_with(
        "so_second_scheduler_thread",
        &["100000000".into()],
        &small_stacks(),
        None,
        false,
    );
    assert_eq!(got.code, "134", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), STACK_OVERFLOW);
}

/// `tasks/wait_any_finished_unnotified` with 20 workers, where natively
/// the outcome is the same (leanrs's probe, 10 of 10): the twin must not
/// run `u` on `main`'s stack either.
#[test]
fn wait_any_finished_unnotified_w20() {
    let id = "wait_any_finished_unnotified";
    let got = run_with(
        id,
        &[],
        &[("LEAN_NUM_THREADS".into(), "20".into())],
        None,
        false,
    );
    let exp = &expected(id)[0];
    assert_eq!(got.code, exp.code, "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), String::from_utf8_lossy(&exp.out));
    assert_eq!(got.err, exp.err);
}

/// Our review of sched-3 (RS3), probe `DedicatedWaiter`, with native's
/// outcome (`LEAN_NUM_THREADS=1`): a dedicated waiter blocks while the
/// queue's head waits for a promise it resolves afterwards.
#[test]
fn rv3_dedicated_waiter() {
    let got = run_with(
        "rv3_dedicated_waiter",
        &[],
        &[("LEAN_NUM_THREADS".into(), "1".into())],
        Some(20),
        false,
    );
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "c ran\nd resolves\nb got 7\nmain done\n");
    assert_eq!(err_of(&got), "");
}

/// Our review of sched-3 (RS3), probe `ChainWait`, with native's outcome
/// (`LEAN_NUM_THREADS=1`): `main` waits for an IO dependent of a task
/// queued behind one that waits for a promise `main` resolves afterwards.
#[test]
fn rv3_chain_wait() {
    let got = run_with(
        "rv3_chain_wait",
        &[],
        &[("LEAN_NUM_THREADS".into(), "1".into())],
        Some(20),
        false,
    );
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(
        out_of(&got),
        "c ran\nd sees 5\nmain resolves\nb got 7\nmain done\n"
    );
    assert_eq!(err_of(&got), "");
}

/// Review HL2-01 (fixes-13), with a pending timer: `main` blocks on a pure
/// bind task while it runs on a context of its own, and the task continues
/// as a new pure task. `main` looks again at once and runs it; before the
/// fix it went on only after the timer (3 s), which keeps the hub from its
/// last resort until then.
#[test]
fn hl2_bind_continued_waiter() {
    let got = run_with(
        "hl2_bind_continued_waiter",
        &[],
        &[("LEAN_NUM_THREADS".into(), "4".into())],
        Some(20),
        false,
    );
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(
        out_of(&got),
        "waitAny: 7\ns: 42, the timer came first: false\n"
    );
    assert_eq!(err_of(&got), "");
}

/// Review RF13-03 (fixes-13), the reviewer's probe: the loop context polls
/// for 2 s when `main` returns at 100 ms; the exit does not wait for its
/// callback to end ("dep done" never comes), as natively, but comes when
/// the loop context's budget ends: the final run's task time (none) plus
/// 1 s, at about 1.1 s.
#[test]
fn rf13_loop_polls_at_exit() {
    let t0 = Instant::now();
    let got = run_with("rf13_loop_polls_at_exit", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "main done\n");
    assert_eq!(err_of(&got), "");
    // the callback reads the clock for 2 s: the exit came before, after
    // `main`'s 100 ms and the budget's 1 s
    assert!(
        t0.elapsed() < Duration::from_millis(1800),
        "{:?}",
        t0.elapsed()
    );
}

/// Review RF13-06 (fixes-13), the reviewer's probe: as above, with an IO
/// task started in each round of the callback. Each task the final run
/// runs adds its time to the loop context's budget, as natively the
/// workers keep running them while the loop thread goes on, so the
/// callback may reach its end (2 s) and print; natively both outcomes
/// occur (LoopPollsAndSpawns.lean, 3 runs: "dep done" at 2.2 s, an exit at
/// 0.1 s without it, and a crash). Here it took 3.3 s with the line. The
/// exit ends either way.
#[test]
fn rf13_loop_polls_and_spawns() {
    let t0 = Instant::now();
    let got = run_with("rf13_loop_polls_and_spawns", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    let out = out_of(&got);
    let rest = out.strip_prefix("main done\n").unwrap_or("?");
    assert!(
        rest.is_empty() || (rest.starts_with("dep done after ") && rest.ends_with(" rounds\n")),
        "stdout {out:?}"
    );
    assert_eq!(err_of(&got), "");
    assert!(t0.elapsed() < Duration::from_secs(15), "{:?}", t0.elapsed());
}

/// Review RF13-04 (fixes-13), the reviewer's probe: after the final run's
/// computing task, the loop context's callback computes 10 ms, then prints;
/// the print's effect point lets `main` go first, and the final run lets
/// the loop context go on again until the callback ends.
#[test]
fn rf13_loop_sleep_compute_print() {
    let got = run_with("rf13_loop_sleep_compute_print", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "main done\nlate\n");
    assert_eq!(err_of(&got), "");
}

/// Review RF13-07 (fixes-13), the reviewer's probe: the loop context's
/// callback starts a 1.5 s task, reads the clock and prints; it goes on
/// alone, and the final run runs the task after it: "late" comes, as
/// natively.
#[test]
fn rf13c_valve_counts_run() {
    let got = run_with("rf13c_valve_counts_run", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "main done\nlate\n");
    assert_eq!(err_of(&got), "");
}

/// Review RF13-08 (fixes-13), the reviewer's probe: a callback that starts
/// a task and sleeps in each round, for 3 s, keeps the exit until it ends,
/// as natively (the workers keep running its tasks). It printed "dep done
/// after 1 rounds" (1 for any positive count).
#[test]
fn rf13c_loop_spawns_and_sleeps() {
    let t0 = Instant::now();
    let got = run_with("rf13c_loop_spawns_and_sleeps", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "main done\ndep done after 1 rounds\n");
    assert_eq!(err_of(&got), "");
    assert!(t0.elapsed() >= Duration::from_millis(3000));
}

/// Review RF13-10 (fixes-13), the reviewer's probe: a callback that spins
/// on a flag a task it started sets goes on once that task has waited
/// `STALE`: "late" comes, and the exit at about 1.1 s (natively the same;
/// the task waited for `main`'s whole deadline before, 3.1 s, and the line
/// was lost).
#[test]
fn rf13d_loop_spins_on_task() {
    let t0 = Instant::now();
    let got = run_with("rf13d_loop_spins_on_task", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "main done\nlate\n");
    assert_eq!(err_of(&got), "");
    assert!(
        t0.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t0.elapsed()
    );
}

/// The other user's review of round 5 (fixes-13): a callback that waits
/// with `IO.wait` in each cycle reports the `sync` task warning once per
/// cycle; natively one line, here a few more before the exit (a documented
/// limit). The exit comes, and stderr holds only those lines.
#[test]
fn rf13f_loop_cycle_wait() {
    let t0 = Instant::now();
    let got = run_with("rf13f_loop_cycle_wait", &[], &[], Some(20), false);
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(out_of(&got), "cycling\nmain done\n");
    let err = err_of(&got);
    let n = err.lines().count();
    assert!(n >= 1, "stderr {err:?}");
    assert!(
        err.lines()
            .all(|l| l == "`Task.get` called from a `(sync := true)` task"),
        "stderr {err:?}"
    );
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
}

/// `tasks/sync_walk_keeps_worker` with 20 workers: `b` runs at once on
/// another worker while the walk's `sync` dependent sleeps, as natively
/// (10 of 10: "B ran", "D done", "main done"; review AR-16).
#[test]
fn sync_walk_keeps_worker_w20() {
    let got = run_with(
        "sync_walk_keeps_worker",
        &[],
        &[("LEAN_NUM_THREADS".into(), "20".into())],
        None,
        false,
    );
    assert_eq!(got.code, "0", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "B ran\nD done\nmain done\n");
    assert!(got.out.is_empty());
}

// The wait cores (batch wait-1; docs/sched.md, "The wait cores"): the
// programs of `src/wait1.rs`, where several contexts wait.

fn w1(id: &str, args: &[&str], env: &[(&str, &str)], hang: Option<u64>) -> Outcome {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    run_with(id, &args, &env, hang, false)
}

/// A program whose output is all on stderr, with status 0.
fn w1_ok(id: &str, args: &[&str], err: &str) {
    let got = w1(id, args, &[], Some(20));
    assert_eq!(got.code, "0", "{id} {args:?}: stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), err, "{id} {args:?}");
    assert!(got.out.is_empty(), "{id} {args:?}");
}

/// Core 3.1: a thunk over a `Gate` forced on three contexts runs its
/// closure once, and its waiters wake in the order they began to wait;
/// D8's probe (leanrs's `thunk_forced_on_two_contexts`).
#[test]
fn w1_gate_forcers() {
    w1_ok(
        "w1_gate_forcers",
        &[],
        "main got 5\nwaiter 1 got 5\nwaiter 2 got 5\nclosure runs: 1\nprobe: 5 6\n",
    );
}

/// Core 3.1: a thunk forced inside its own closure hangs while the others
/// go on (LB-08), and the process waits for it at exit, as `hang()`'s.
#[test]
fn w1_gate_self_force() {
    w1_ok(
        "w1_gate_self_force",
        &[],
        "main: the task hangs in its own force\n",
    );
    let got = w1("w1_gate_self_force", &["exit"], &[], Some(2));
    assert_eq!(got.code, "timeout", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "main: the task hangs in its own force\n");
}

/// Core 3.1: a static over a `Gate` read on two contexts while its
/// initializer waits (leanrs's `local_lazy_read_while_init_suspended`).
#[test]
fn w1_static_two_readers() {
    w1_ok(
        "w1_static_two_readers",
        &[],
        "main 9, reader 9, initializer runs 1\n",
    );
}

/// Core 3.1, the keyed claims of lean2rr's constants (odd keys): two
/// readers, one run; the initializer's own read hangs.
#[test]
fn w1_keyed_constant() {
    w1_ok(
        "w1_keyed_constant",
        &[],
        "main 11, reader 11, initializer runs 1\n",
    );
    w1_ok(
        "w1_keyed_constant",
        &["self"],
        "main: the initializer hangs in its own read\n",
    );
}

/// Core 3.1, AR-42: three waiters on a keyed entry kept past the table's 8
/// places, which moves in its `Vec` while they wait, wake at its store in
/// the order they began to wait (W1).
#[test]
fn w1_keyed_spilled_waiters() {
    w1_ok(
        "w1_keyed_spilled_waiters",
        &[],
        "main got 17\nreader 1 got 17\nreader 2 got 17\nreader 3 got 17\ninitializer runs 1\n",
    );
}

/// Core 3.1, lean2rr's `busy` thunk (`wait_running_keyed`): another context
/// waits for its store; forced by its own closure with no other live
/// context, or before the task manager runs, it hangs at once.
#[test]
fn w1_busy_thunk() {
    w1_ok("w1_busy_thunk", &[], "main 13, reader 13\n");
    let got = w1("w1_busy_thunk", &["alone"], &[], Some(2));
    assert_eq!(got.code, "timeout", "stderr {:?}", err_of(&got));
    assert_eq!(err_of(&got), "the closure forces its own thunk, alone\n");
    let got = w1("w1_busy_thunk", &[], &[("W1_BEFORE", "1")], Some(2));
    assert_eq!(got.code, "timeout", "stderr {:?}", err_of(&got));
    assert_eq!(
        err_of(&got),
        "before the task manager: the closure forces its thunk\n"
    );
}

/// W3 through the `extern "C"` keyed functions: a wait inside a no-suspend
/// scope is a Rust panic with the reason, which aborts there (status 134).
#[test]
fn w1_w3_keyed() {
    for a in ["busy", "step", "ref"] {
        let got = w1("w1_w3_keyed", &[a], &[], Some(10));
        let err = err_of(&got);
        assert_eq!(got.code, "134", "{a}: stderr {err:?}");
        assert!(
            err.contains("lean-runtime: a wait inside a no-suspend scope (a free)"),
            "{a}: stderr {err:?}"
        );
        assert!(!err.contains("not reached"), "{a}");
    }
}

/// Core 3.2, the keyed form's frame rule: modify's own store closes the
/// take and wakes the waiters in order; a store from a `sync` dependent
/// nested in modify's function, or from a task run on the taker's stack,
/// a nested take, and the taker's own `get` (RS4-01) wait (forever, here).
#[test]
fn w1_ref_keyed() {
    w1_ok(
        "w1_ref_keyed",
        &["close"],
        "reader got 42\nmain got 42\nafter modify: 42\n",
    );
    for a in ["dep_store", "stack_task", "nested_take", "own_get"] {
        w1_ok("w1_ref_keyed", &[a], &format!("main: {a} waits\n"));
    }
}

/// Core 3.3 and W3 (lean2rr's L6): a promise dropped in a free, whose
/// `sync` dependent reaches a wait core (a reference a task's `modify`
/// holds), is resolved after the drain: the dependent waits there, outside
/// the no-suspend scope, and gets the stored value.
#[test]
fn w1_dependent_waits() {
    w1_ok(
        "w1_dependent_waits",
        &[],
        "dependent got 2\nmain: after the free\nafter modify: 2\n",
    );
}

/// Core 3.3: a deferred resolution whose dependent hangs leaves the rest of
/// its walk pending (R5, the in-flight count), and a later drain on another
/// context resolves in full; no entry is queued at a switch (R6).
#[test]
fn w1_drain_hang() {
    w1_ok(
        "w1_drain_hang",
        &[],
        "main: pending true\nZ\nmain: pending true\n",
    );
}

/// Review NEW-1 of wait-1: a contended cooperative `flock` hands off at
/// the unlock itself: the waiter's last wait ended with `main`'s unlock,
/// not with its nap running out (which made every handoff up to 16 ms
/// late). The waiter records how its wait ended, so no clock is read.
#[test]
fn w1_flock_handoff() {
    w1_ok(
        "w1_flock_handoff",
        &[],
        "the waiter's flock ended with: Unlock\n",
    );
}

/// Core 3.2: 3000 random schedules of `ST.Ref` operations on contexts of
/// their own, against Lean 4.35's store model (leanrs's evidence
/// `beh_ref_empty_cell.rs`, on the real types), for the object form and
/// the keyed form.
#[test]
fn w1_ref_schedules() {
    for form in ["object", "keyed"] {
        let got = w1(
            "w1_ref_schedules",
            &[form],
            &[("LEAN_STACK_SIZE_KB", "256")],
            Some(120),
        );
        let err = err_of(&got);
        assert_eq!(got.code, "0", "{form}: stderr {err:?}");
        assert!(
            err.starts_with(&format!(
                "{form} form: 3000 schedules agree with Lean 4.35's store model\n"
            )),
            "{form}: stderr {err:?}"
        );
    }
}

/// The ported cases, in one list: each becomes a test, and
/// `every_case_is_ported` checks the list against the cases' directories
/// (review RS1S-06 of sched-1).
macro_rules! cases {
    ($($id:ident),* $(,)?) => {
        const PORTED: &[&str] = &[$(stringify!($id)),*];
        $(
            #[test]
            fn $id() {
                check(stringify!($id));
            }
        )*
    };
}

cases!(
    checkcanceled_after_main,
    worker_keeps_streams,
    worker_keeps_errno,
    dropped_pure_task,
    exit_joins_before_flush,
    hasfinished_spin,
    runaway_io_task_unawaited,
    runaway_pure_task_referenced,
    runaway_pure_task_started,
    sleep_polling_spin,
    sync_dependent_order,
    promise_across_tasks,
    stack_overflow_in_task,
    mutex_handoff,
    condvar_turns,
    shared_mutex_readers,
    recursive_mutex,
    pure_chain_io_dep,
    pure_bind_io_dep,
    exit_from_task,
    get_in_sync_task,
    promise_result_opt,
    result_bang_some,
    result_bang_dropped,
    result_bang_dropped_in_task,
    result_bang_dropped_abort,
    promise_in_initialize,
    promise_in_initialize_abort,
    result_bang_dropped_first,
    task_pure_graph,
    result_bang_dropped_redirected,
    get_in_sync_task_redirected,
    cancel_promise_and_pure,
    result_bang_dep_order,
    pure_get_in_sync_task,
    sync_dependent_before_waiter,
    dropped_promise_waiter_wakes,
    dropped_promise_waiter_unrelated_finish,
    waiter_wakes_after_nested_finish,
    sync_walk_stuck_unrelated_finish,
    wait_any_wakes_on_finish,
    sync_walk_mutex_unrelated_finish,
    sync_walk_mutex_alone,
    sync_walk_stuck_alone,
    sync_walk_mutex_unref_finish,
    wait_any_unref_finish,
    wait_any_pure_stalled,
    // sched-3: what a waiter or a poller runs on its own stack (AR-9, AR-10)
    wait_queue_order,
    wait_head_blocks_on_main,
    wait_any_head_blocks_on_main,
    wait_any_keeps_worker,
    poll_queue_order,
    poll_threshold_promise,
    poll_threshold_mutex,
    wait_picked_pure,
    wait_any_picked_pure,
    wait_any_finished_unnotified,
    // sched-4 (AR-15)
    self_wait_frees_worker,
    runaway_pure_task_before_io,
    sync_walk_keeps_worker,
    sync_self_wait_keeps_worker,
    sync_wait_in_inline_walk,
    sync_dep_waits_queued_task,
    // fixes-3 (AR-25)
    wait_pure_queue_order,
    drop_queued_behind_pure,
    runaway_pure_before_awaited,
    // the review of fixes-3 (RF3)
    picked_task_own_worker_streams,
    picked_task_sleeping_worker,
    picked_task_ticking_worker,
    picked_task_reaches_yield_points,
    runaway_pure_passed_over,
    // review AR-33
    worker_streams_closed_at_exit,
    worker_streams_at_process_exit,
    // review AR-34
    worker_streams_before_dedicated,
    // reviews RF3-05, LF3-04, LF3-05
    picked_task_short_sleeper_long,
    picked_task_watchdog,
    picked_task_sleep_zero,
    late_task_after_main,
    late_dependent_of_dedicated,
    late_wait_dedicated,
    late_wait_pool,
    late_pool_child_runs,
    late_dedicated_child_runs,
    main_waits_dedicated_child,
    wait_dep_mid_walk,
    wait_dep_mid_promise_walk,
    poll_dep_mid_walk,
    wait_any_own_dep,
    task_waits_own_dep,
    sync_dep_waits_older,
    lost_update,
    set_during_modify,
    get_during_modify,
    own_get_during_modify,
    promise_nested_free_order,
    swap_during_modify,
    output_big_stdout,
    output_both_overflow,
    task_reads_main_writes,
    wait_in_task,
    output_while_ticking,
    output_input_while_ticking,
    // threads mode, batch T3: cases that need real contention
    wait_chain_beyond_pool,
    wait_any_faster,
    stack_overflow_in_dedicated,
    late_tasks_while_enqueuing,
    get_tid_threads,
    big_priority_dedicated,
    // fixes-13: HL2-01, HL2-03
    wait_bind_continued_elsewhere,
    wait_chain_bind_continued,
    loop_configure,
    timer_oneshot,
    get_tid_loop_thread,
    loop_blocked_at_exit,
    loop_task_at_exit,
    loop_sleep_expired,
    loop_polls_at_exit,
    loop_sleep_poll_print,
    loop_sleep_compute_print,
    loop_valve_counts_run,
    loop_spawn_poll,
    loop_poll_work,
    loop_spins_on_task,
    loop_cycle,
    loop_sleeps_after_task,
    loop_cycle_long,
    loop_carried_then_poll,
    timer_repeating,
    timer_cancel_reset,
    signal_usr1,
    signal_stale,
    signal_oneshot_twice,
    signal_failed_next,
    signal_stale_deferred,
    timer_due_stop,
    signal_cancel_restart,
    signal_order,
    signal_fds,
    exit_listening,
    signal_sigio_default,
    timer_stop_in_sync_dependent,
    timer_cancel_in_sync_dependent,
    signal_stop_in_sync_dependent,
    signal_cancel_in_sync_dependent,
    timer_catchup_bound,
    timer_oneshot_stop_resubscribe,
    timer_stop_rearm_in_sync_dependent,
    timer_cancel_rearm_in_sync_dependent,
    timer_oneshot_stop_keep_in_sync_dependent,
    timer_oneshot_cancel_keep_in_sync_dependent,
    timer_oneshot_cancel_resubscribe,
    timer_stop_rearm_async_dependent,
    signal_stop_rearm_in_sync_dependent,
    signal_cancel_rearm_in_sync_dependent,
    signal_oneshot_stop_keep_in_sync_dependent,
    signal_oneshot_cancel_keep_in_sync_dependent,
    signal_stop_drops_promise,
    signal_rearm_in_sync_dependent,
    signal_rearm_in_async_dependent,
    signal_reset_urg_in_sync_dependent,
    signal_reset_winch_in_sync_dependent,
    signal_reset_usr1_in_sync_dependent,
    signal_reset_urg_in_async_dependent,
    signal_reset_usr1_after_repeating_stop,
    signal_reset_urg_after_repeating_stop,
    lock_blocked,
    lock_exit,
    lock_during_read,
    // tests/cases/net (net-1)
    tcp_echo,
    tcp_errors,
    tcp_v6,
    udp_basic,
    udp_errors,
    dns_localhost,
    dns_pending_at_exit,
    iface_lo,
    accept_parallel,
    accept_parallel_try,
    keepalive_zero_delay,
    multicast_ipv6_long,
    recv_huge_overflow,
    recv_huge_oom,
    udp_recv_huge_overflow,
    udp_cancel_recv_leak,
    tcp_shutdown_fail_leak,
    recv_zero_eof,
    recv_zero_data_eof,
    recv_zero_data,
    udp_recv_zero,
    shutdown_during_connect,
    shutdown_after_queued_write,
    shutdown_after_connect,
    // tests/cases/net (net-threads)
    clients_in_tasks,
    socket_across_tasks,
    extern_in_sync_dependent,
    exit_while_reading,
    exit_while_writing,
    exit_while_writing_stalled,
    handoff_then_resolve,
    handoff_then_write,
    handoff_then_kill,
);
