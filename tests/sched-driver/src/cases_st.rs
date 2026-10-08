//! The Rust ports that run over the single-thread scheduler only
//! (`sched-cases`): the cases of `tests/cases/uvloop` (threads mode runs
//! them in the crate's `tests/threads_twins.rs`), the io and process cases
//! with tasks (sched-io's cooperative IO and the stream hand-offs), and
//! programs that are not Lean programs (Rust panics in a context, leanrs's
//! adversarial checks of `Glue::suspend`). `cases.rs` has the ports both
//! drivers share.

use crate::cases::{ok, Case};
use crate::glue::{eprintln, println};
use crate::lean::*;
use crate::lio::{self, quote, R};
use lean_runtime::io::process::{Stdio, StdioConfig};
use lean_runtime::io::{FsMode, Handle};
use std::rc::Rc;

/// The single-thread driver's own programs, by id; then the `net` cases.
pub fn lookup(id: &str) -> Option<Case> {
    fn no_init() {}
    Some(match id {
        // tests/cases/uvloop: Std.Internal.UV's loop, timers and signals
        "loop_configure" => (no_init, loop_configure),
        "timer_oneshot" => (no_init, timer_oneshot),
        "timer_repeating" => (no_init, timer_repeating),
        "timer_cancel_reset" => (no_init, timer_cancel_reset),
        "signal_usr1" => (no_init, signal_usr1),
        "signal_stale" => (no_init, signal_stale),
        "signal_oneshot_twice" => (no_init, signal_oneshot_twice),
        "signal_failed_next" => (no_init, signal_failed_next),
        "signal_stale_deferred" => (no_init, signal_stale_deferred),
        "timer_due_stop" => (no_init, timer_due_stop),
        "signal_cancel_restart" => (no_init, signal_cancel_restart),
        "signal_order" => (no_init, signal_order),
        "signal_fds" => (no_init, signal_fds),
        "exit_listening" => (no_init, exit_listening),
        "signal_sigio_default" => (no_init, signal_sigio_default),
        "timer_stop_in_sync_dependent" => (no_init, lb20_probe),
        "timer_cancel_in_sync_dependent" => (no_init, lb20_probe),
        "signal_stop_in_sync_dependent" => (no_init, lb20_probe),
        "signal_cancel_in_sync_dependent" => (no_init, lb20_probe),
        "timer_catchup_bound" => (no_init, timer_catchup_bound),
        "timer_oneshot_stop_resubscribe" => (no_init, timer_oneshot_stop_resubscribe),
        "timer_stop_rearm_in_sync_dependent" => (no_init, timer_stop_rearm_in_sync_dependent),
        "timer_cancel_rearm_in_sync_dependent" => (no_init, timer_cancel_rearm_in_sync_dependent),
        "timer_oneshot_stop_keep_in_sync_dependent" => {
            (no_init, timer_oneshot_keep_in_sync_dependent)
        }
        "timer_oneshot_cancel_keep_in_sync_dependent" => {
            (no_init, timer_oneshot_keep_in_sync_dependent)
        }
        "timer_oneshot_cancel_resubscribe" => (no_init, timer_oneshot_cancel_resubscribe),
        "timer_stop_rearm_async_dependent" => (no_init, timer_stop_rearm_async_dependent),
        "signal_stop_rearm_in_sync_dependent" => (no_init, signal_stop_rearm_in_sync_dependent),
        "signal_cancel_rearm_in_sync_dependent" => (no_init, signal_cancel_rearm_in_sync_dependent),
        "signal_oneshot_stop_keep_in_sync_dependent" => {
            (no_init, signal_oneshot_keep_in_sync_dependent)
        }
        "signal_oneshot_cancel_keep_in_sync_dependent" => {
            (no_init, signal_oneshot_keep_in_sync_dependent)
        }
        "signal_stop_drops_promise" => (no_init, signal_stop_drops_promise),
        "signal_rearm_in_sync_dependent" => (no_init, signal_rearm_in_dependent),
        "signal_rearm_in_async_dependent" => (no_init, signal_rearm_in_dependent),
        "signal_reset_urg_in_sync_dependent" => (no_init, signal_reset_in_dependent),
        "signal_reset_winch_in_sync_dependent" => (no_init, signal_reset_in_dependent),
        "signal_reset_usr1_in_sync_dependent" => (no_init, signal_reset_in_dependent),
        "signal_reset_urg_in_async_dependent" => (no_init, signal_reset_in_dependent),
        "signal_reset_usr1_after_repeating_stop" => (no_init, signal_reset_after_repeating_stop),
        "signal_reset_urg_after_repeating_stop" => (no_init, signal_reset_after_repeating_stop),
        "get_tid_loop_thread" => (no_init, get_tid_loop_thread),
        // fixes-16: hunt HSK-03
        "loop_deep_sync_dependent" => (no_init, loop_deep_sync_dependent),
        "loop_blocked_at_exit" => (no_init, loop_blocked_at_exit),
        "loop_task_at_exit" => (no_init, loop_task_at_exit),
        "loop_sleep_expired" => (no_init, loop_sleep_expired),
        "loop_polls_at_exit" => (no_init, loop_polls_at_exit),
        "loop_sleep_poll_print" => (no_init, loop_sleep_poll_print),
        "loop_sleep_compute_print" => (no_init, loop_sleep_compute_print),
        "loop_valve_counts_run" => (no_init, loop_valve_counts_run),
        "loop_spawn_poll" => (no_init, loop_spawn_poll),
        "loop_poll_work" => (no_init, loop_poll_work),
        "loop_spins_on_task" => (no_init, loop_spins_on_task),
        "loop_cycle" => (no_init, loop_cycle),
        "loop_sleeps_after_task" => (no_init, loop_sleeps_after_task),
        "loop_cycle_long" => (no_init, loop_cycle_long),
        "loop_carried_then_poll" => (no_init, loop_carried_then_poll),
        // fixes-14: AR-52, HU-01..06
        "timer_due_in_final_run" => (no_init, timer_due_in_final_run),
        "timer_chain_in_final_run" => (no_init, timer_chain_in_final_run),
        "timer_effect_order" => (no_init, timer_effect_order),
        "timer_repeat_zero_held" => (no_init, timer_repeat_zero_held),
        "timer_fresh_next_twice" => (no_init, timer_fresh_next_twice),
        "signal_before_timer_in_look" => (no_init, signal_before_timer_in_look),
        "signal_batch_new_watcher" => (no_init, signal_batch_new_watcher),
        "timer_period_from_look" => (no_init, timer_period_from_look),
        // tests/cases/io: the cases with tasks
        "lock_blocked" => (no_init, lock_blocked),
        "lock_exit" => (no_init, lock_exit),
        "lock_during_read" => (no_init, lock_during_read),
        // tests/cases/process: the cases with tasks
        "exit_while_reading" => (no_init, exit_while_reading),
        "exit_while_writing" => (no_init, exit_while_writing),
        "exit_while_writing_stalled" => (no_init, exit_while_writing),
        "handoff_then_resolve" => (no_init, handoff_then_resolve),
        "handoff_then_write" => (no_init, handoff_then_write),
        "handoff_then_kill" => (no_init, handoff_then_kill),
        // fixes-14: HR-01..03
        "handoff_then_resolve_again" => (no_init, handoff_then_resolve_again),
        "handoff_then_sync_map" => (no_init, handoff_then_sync_map),
        "handoff_in_tree_then_sync_map" => (no_init, handoff_in_tree_then_sync_map),
        "handoff_then_try_lock" => (no_init, handoff_then_try_lock),
        // fixes-14 round 2: RF14-07
        "deferred_resolve_before_handoff" => (no_init, deferred_resolve_before_handoff),
        // Not Lean programs: the regression programs of sched-io's reviews.
        "rsio_poll_fds_with_stream_lock" => {
            (no_init, crate::review::rsio_poll_fds_with_stream_lock)
        }
        "rsio_watch_spin" => (no_init, crate::review::rsio_watch_spin),
        "rsio_drop_no_suspend" => (no_init, crate::review::rsio_drop_no_suspend),
        "rsio_ns_drop_deadlock" => (no_init, crate::review::rsio_ns_drop_deadlock),
        "rsio_ns_cat" => (no_init, crate::review::rsio_ns_cat),
        "rsio_ns_leak" => (no_init, crate::review::rsio_ns_leak),
        "rsio_ns_leak_panic" => (no_init, crate::review::rsio_ns_leak_panic),
        "rsio_ns_partial" => (no_init, crate::review::rsio_ns_partial),
        "rsio_ns_unwind" => (no_init, crate::review::rsio_ns_unwind),
        "rsio_ns_force_exit" => (no_init, crate::review::rsio_ns_force_exit),
        "rfx1_shared_read" => (no_init, crate::review::rfx1_shared_read),
        "rfx1_trywait" => (no_init, crate::review::rfx1_trywait),
        "rsio_exit_join" => (no_init, crate::review::rsio_exit_join),
        "rfx2_exit_handoff" => (no_init, crate::review::rfx2_exit_handoff),
        "rfx2_exit_unrelated_handoff" => (no_init, crate::review::rfx2_exit_unrelated_handoff),
        "rfx2_exit_writer_held" => (no_init, crate::review::rfx2_exit_writer_held),
        "rfx2_causal_handoff" => (no_init, crate::review::rfx2_causal_handoff),
        "rfx3_promise_handoff" => (no_init, crate::review::rfx3_promise_handoff),
        "rfx3_exit_contexts" => (no_init, crate::review::rfx3_exit_contexts),
        "rfx4_fs_signal" => (no_init, crate::review::rfx4_fs_signal),
        "rfx4_fd_after" => (no_init, crate::review::rfx4_fd_after),
        // AR-11: Lean's stack-overflow report, owned by the crate.
        "so_main_overflow" => (no_init, crate::review::so_main_overflow),
        "so_task_overflow_after_switches" => {
            (no_init, crate::review::so_task_overflow_after_switches)
        }
        "so_segv_in_task" => (no_init, crate::review::so_segv_in_task),
        "so_prev_resethand" => (no_init, crate::review::so_prev_resethand),
        // Probes of our review of sched-3 (RS3).
        "rv3_dedicated_waiter" => (no_init, crate::review::rv3_dedicated_waiter),
        "rv3_chain_wait" => (no_init, crate::review::rv3_chain_wait),
        // Reviews HL2-01 and RF13-03 (fixes-13).
        "hl2_bind_continued_waiter" => (no_init, crate::review::hl2_bind_continued_waiter),
        "rf13_loop_polls_at_exit" => (no_init, crate::review::rf13_loop_polls_at_exit),
        "rf13_loop_polls_and_spawns" => (no_init, crate::review::rf13_loop_polls_and_spawns),
        "rf13_loop_sleep_compute_print" => (no_init, crate::review::rf13_loop_sleep_compute_print),
        "rf13c_valve_counts_run" => (no_init, crate::review::rf13c_valve_counts_run),
        "rf13c_loop_spawns_and_sleeps" => (no_init, crate::review::rf13c_loop_spawns_and_sleeps),
        "rf13d_loop_spins_on_task" => (no_init, crate::review::rf13d_loop_spins_on_task),
        "rf13f_loop_cycle_wait" => (no_init, crate::review::rf13f_loop_cycle_wait),
        // Review RF16-03 (fixes-17).
        "rf16_started_task_holds_worker" => {
            (no_init, crate::review::rf16_started_task_holds_worker)
        }
        "so_rust_thread_overflow" => (no_init, crate::review::so_rust_thread_overflow),
        "so_second_scheduler_thread" => (no_init, crate::review::so_second_scheduler_thread),
        // Not Lean programs: the wait cores where several contexts wait
        // (batch wait-1).
        "w1_gate_forcers" => (no_init, crate::wait1::w1_gate_forcers),
        "w1_gate_self_force" => (no_init, crate::wait1::w1_gate_self_force),
        "w1_static_two_readers" => (no_init, crate::wait1::w1_static_two_readers),
        "w1_keyed_constant" => (no_init, crate::wait1::w1_keyed_constant),
        "w1_keyed_spilled_waiters" => (no_init, crate::wait1::w1_keyed_spilled_waiters),
        "w1_busy_thunk" => (crate::wait1::w1_busy_init, crate::wait1::w1_busy_thunk),
        "w1_w3_keyed" => (no_init, crate::wait1::w1_w3_keyed),
        "w1_ref_keyed" => (no_init, crate::wait1::w1_ref_keyed),
        "w1_dependent_waits" => (no_init, crate::wait1::w1_dependent_waits),
        "w1_drain_hang" => (no_init, crate::wait1::w1_drain_hang),
        "w1_ref_schedules" => (no_init, crate::wait1::w1_ref_schedules),
        "w1_flock_handoff" => (no_init, crate::wait1::w1_flock_handoff),
        // Not a Lean program: a Rust panic (a translator's or the runtime's
        // bug) in a task on a context of its own.
        "rust_panic_in_task" => (no_init, rust_panic_in_task),
        "rust_panic_through_effect" => (no_init, rust_panic_through_effect),
        "effect_points_in_a_started_task" => (no_init, effect_points_in_a_started_task),
        // Not Lean programs either: leanrs's adversarial checks of
        // docs/sched.md's "Why Glue::suspend is sound" (S5, S6).
        "adv_block_in_drop_during_unwind" => (no_init, adv_block_in_drop_during_unwind),
        "adv_panic_in_sync_dep_of_drop" => (no_init, adv_panic_in_sync_dep_of_drop),
        "adv_exit_from_task" => (no_init, adv_exit_from_task),
        // Not a case of `tests/cases`: AR-39 (lean2rr's review RS7-02), an
        // initializer keeps a recursive mutex locked and `main` locks it
        // again on the same OS thread.
        "rs7_init_reclock" => (rs7_init_reclock_init, rs7_init_reclock),
        // Not a case of `tests/cases`: review RF14-03, a `sync` dependent that
        // `depend` runs at once is Lean's fast path, with no panic.
        "rf14_depend_fast_path" => (no_init, rf14_depend_fast_path),
        "rf14_fast_pool_caller_waits" => (no_init, rf14_fast_pool_caller_waits),
        // tests/cases/net: networking (net-1)
        _ => return crate::netcases::lookup(id),
    })
}

// ---------------------------------------------------------------------------
// Not Lean programs: a Rust panic (a translator's or the runtime's bug) in a
// task on a context of its own (docs/sched.md, "Rust panics").

fn rust_panic_in_task(_: &[String]) -> u32 {
    println("main starts");
    let _ = as_task(|| -> () { panic!("a Rust panic in a task") }, PRIO_DEFAULT);
    sleep(1000);
    eprintln("not reached");
    0
}

/// As `rust_panic_in_task`, but `main` is in an effect point when the task
/// panics: the panic resumes in the effect point's cold path, which cannot
/// unwind (review AR-28), so the process aborts there.
fn rust_panic_through_effect(_: &[String]) -> u32 {
    println("main starts");
    let _ = as_task(|| -> () { panic!("a Rust panic in a task") }, PRIO_DEFAULT);
    // no yield point: at the output, the task has been queued for 10 ms
    // (`STALE` is 5), so the effect point lets it go first
    std::thread::sleep(std::time::Duration::from_millis(10));
    eprintln("not reached");
    0
}

// ---------------------------------------------------------------------------
// AR-39 (lean2rr's review RS7-02, its probe `RS7Init` with `RS7=reclock`): a
// lock's owner is the OS thread, not whether the scheduler has started. Not
// a case of `tests/cases`: natively the outcome depends on
// `LEAN_MAIN_USE_THREAD` (on a thread of its own, `main` waits forever).
// `tests/cases.rs` runs it with `LEAN_MAIN_USE_THREAD=0`, where the glue
// runs `main` on the initializers' thread, as natively; `HELD`, a
// thread-local of that thread, stands for the initialized constant.
//
// initialize held : BaseRecursiveMutex ← do
//   let r ← BaseRecursiveMutex.new
//   r.lock
//   pure r
//
// def tryInTask (what : String) : IO Unit := do
//   let t ← IO.asTask (prio := .dedicated) do
//     let ok ← held.tryLock
//     IO.println s!"{what}: {ok}"
//     if ok then held.unlock
//   let _ ← IO.wait t
//
// def main : IO Unit := do
//   IO.println s!"main tryLock: {← held.tryLock}"
//   held.lock
//   IO.println "main has the lock"
//   tryInTask "task tryLock"
//   held.unlock; held.unlock; held.unlock
//   tryInTask "task tryLock after the unlocks"

thread_local! {
    static HELD: std::cell::RefCell<Option<Obj<lean_runtime::sched::sync::RecursiveMutex>>> =
        const { std::cell::RefCell::new(None) };
}

fn rs7_init_reclock_init() {
    let r = Obj::new(lean_runtime::sched::sync::RecursiveMutex::new());
    r.lock();
    HELD.with(|h| *h.borrow_mut() = Some(r));
}

fn rs7_init_reclock(_: &[String]) -> u32 {
    let held = HELD
        .with(|h| h.borrow().clone())
        .expect("the initializer ran");
    let try_in_task = |what: &'static str| {
        let h = held.clone();
        as_task(
            move || {
                let ok = h.try_lock();
                println(&format!("{what}: {ok}"));
                if ok {
                    h.unlock();
                }
            },
            PRIO_DEDICATED,
        )
        .get()
    };
    println(&format!("main tryLock: {}", held.try_lock()));
    held.lock();
    println("main has the lock");
    try_in_task("task tryLock");
    held.unlock();
    held.unlock();
    held.unlock();
    try_in_task("task tryLock after the unlocks");
    0
}

// ---------------------------------------------------------------------------
// leanrs's adversarial checks of docs/sched.md's "Why Glue::suspend is
// sound" (their proof check of sched-1 at 15b62ed, 2026-10-03), ported as
// regression tests. Not Lean programs: Rust panics and `process::exit` from
// a context.

/// A promise held by a value that a panic unwinds (its drop resolves the
/// promise with `none`).
struct Holder(#[allow(dead_code)] Promise<u32>);

/// A task on its own context panics while holding the last reference to a
/// promise. Dropping it resolves the promise with `none`, which runs its
/// `sync` dependent there and then; the dependent waits for a sleeping task,
/// so the context suspends from inside a cleanup while a panic is in flight
/// (S6). The panic then goes on in `main`: status 101.
fn adv_block_in_drop_during_unwind(_: &[String]) -> u32 {
    println("main starts");
    let p: Promise<u32> = Promise::new();
    let slow = as_task(
        || {
            sleep(200);
            7u32
        },
        PRIO_DEFAULT,
    );
    let slow2 = slow.clone();
    let _dep = map_task(
        move |v: Option<u32>| {
            eprintln("sync dep runs in drop during unwind");
            let w = slow2.get();
            eprintln(&format!("sync dep got {w} after block, v = {v:?}"));
            w
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let _ = as_task::<()>(
        move || {
            let _h = Holder(p);
            panic!("boom in task")
        },
        PRIO_DEFAULT,
    );
    sleep(1000);
    eprintln("not reached");
    0
}

/// As above, but the `sync` dependent run from the destructor panics itself:
/// a panic in a destructor during unwinding, which aborts, as in plain Rust.
fn adv_panic_in_sync_dep_of_drop(_: &[String]) -> u32 {
    println("main starts");
    let p: Promise<u32> = Promise::new();
    let _dep = map_task(
        move |_v: Option<u32>| -> u32 { panic!("second panic in sync dep") },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let _ = as_task::<()>(
        move || {
            let _h = Holder(p);
            panic!("boom in task")
        },
        PRIO_DEFAULT,
    );
    sleep(1000);
    eprintln("not reached");
    0
}

/// `IO.Process.exit` inside a task running on a context: glibc's `exit` runs
/// the thread's TLS destructors (the scheduler's) on the coroutine's stack;
/// they only forget suspended contexts and pending jobs (S5). Status 3, the
/// buffered stdout lost as the glue does not flush it here.
fn adv_exit_from_task(_: &[String]) -> u32 {
    println("main starts");
    let keep: Task<u32> = as_task(
        || {
            sleep(5000);
            1
        },
        PRIO_DEFAULT,
    );
    let _ = as_task::<()>(
        move || {
            let _k = keep.clone();
            eprintln("task exits");
            lean_runtime::sched::effect();
            std::process::exit(3)
        },
        PRIO_DEFAULT,
    );
    sleep(1000);
    eprintln("not reached");
    0
}

// ---------------------------------------------------------------------------
// The io and process cases with tasks: blocking IO on one thread (sched-io),
// and the writer hand-offs of dropped streams.

// tests/cases/io/lock_blocked.lean
fn lock_blocked(args: &[String]) -> u32 {
    let f = args[0].clone();
    ok(lio::write_file(&f, ""));
    let a = ok(Handle::open(f.as_bytes(), FsMode::ReadWrite));
    let b = ok(Handle::open(f.as_bytes(), FsMode::ReadWrite));
    ok(a.lock(true));
    let tb = b.clone();
    let t = as_task(
        move || -> R<()> {
            tb.lock(true)?;
            println("task: b locked");
            Ok(())
        },
        PRIO_DEDICATED,
    );
    sleep(to_nat(&args[1]) as u32);
    ok(b.put_str(b"x"));
    ok(b.flush());
    println("main: wrote through b while the task waits in b.lock");
    ok(a.unlock());
    let _ = t.get();
    println(&format!("done; file {}", quote(&ok(lio::read_file(&f)))));
    0
}

// tests/cases/io/lock_exit.lean
fn lock_exit(args: &[String]) -> u32 {
    let f = args[0].clone();
    ok(lio::write_file(&f, ""));
    let a = ok(Handle::open(f.as_bytes(), FsMode::ReadWrite));
    let b = ok(Handle::open(f.as_bytes(), FsMode::ReadWrite));
    ok(a.lock(true));
    let tb = b.clone();
    let _t = as_task(
        move || -> R<()> {
            tb.lock(true)?;
            println("task: b locked");
            Ok(())
        },
        PRIO_DEDICATED,
    );
    sleep(to_nat(&args[1]) as u32);
    ok(b.put_str(b"y"));
    println("main: exiting with the task still in b.lock");
    let _held = (&a, &b);
    crate::glue::process_exit(0)
}

// tests/cases/process/exit_while_reading.lean (LB-29: the exit does not wait
// for the task's read)
fn exit_while_reading(args: &[String]) -> u32 {
    let secs = args.first().cloned().unwrap_or_else(|| "10".to_owned());
    let _t = as_task(
        move || -> R<()> {
            let child = lio::spawn(
                "sleep",
                &[&secs],
                StdioConfig {
                    stdin: Stdio::Null,
                    stdout: Stdio::Piped,
                    stderr: Stdio::Null,
                },
            )?;
            let s = lio::read_to_end(child.stdout.as_ref().expect("piped"))?;
            println(&format!("read {}", s.chars().count()));
            Ok(())
        },
        PRIO_DEDICATED,
    );
    sleep(300);
    println("exiting with 3");
    lean_runtime::sched::effect();
    ok(Handle::stdout().flush());
    crate::glue::process_exit(3)
}

// tests/cases/process/exit_while_writing{,_stalled}.lean (LB-29's scope: the
// exit waits for a writer, here cooperatively)
fn exit_while_writing(args: &[String]) -> u32 {
    let (mode, size, out) = (args[0].clone(), to_nat(&args[1]) as usize, args[2].clone());
    let script = format!(
        "import sys, time\nn = 0\nwhile True:\n    b = sys.stdin.buffer.read1(65536)\n    if not b: break\n    n += len(b)\n    time.sleep(0.05)\nopen('{out}', 'w').write(str(n))\n"
    );
    let (cmd, cargs): (&str, Vec<&str>) = if mode == "slow" {
        ("python3", vec!["-c", &script])
    } else {
        ("sleep", vec!["30"])
    };
    let child = ok(lio::spawn(
        cmd,
        &cargs,
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    ));
    let stdin = child.stdin.expect("piped");
    let _t = as_task(
        move || -> R<()> {
            stdin.write(&vec![120u8; size])?;
            stdin.flush()
        },
        PRIO_DEDICATED,
    );
    sleep(300);
    println(&format!("exiting with 3 ({size} bytes being written)"));
    lean_runtime::sched::effect();
    ok(Handle::stdout().flush());
    crate::glue::process_exit(3)
}

// tests/cases/process/handoff_then_resolve.lean (AR-8: the task's resolution
// waits for its handed-off stream's writer, as natively its `fclose`)
fn handoff_then_resolve(args: &[String]) -> u32 {
    let n = to_nat(&args[0]) as usize;
    let p: Rc<Promise<()>> = Rc::new(Promise::new());
    let p2 = p.clone();
    let _a = as_task(
        move || -> R<()> {
            let child = lio::spawn(
                "sh",
                &["-c", "sleep 1; cat > out; echo done > marker"],
                StdioConfig {
                    stdin: Stdio::Piped,
                    stdout: Stdio::Inherit,
                    stderr: Stdio::Inherit,
                },
            )?;
            let stdin = child.stdin.expect("piped");
            stdin.put_str(&vec![b'a'; n])?;
            {
                // the translator's free path
                let _scope = lean_runtime::sched::no_suspend();
                drop(stdin);
            }
            p2.resolve(());
            Ok(())
        },
        PRIO_DEDICATED,
    );
    let _ = p.result_opt().get();
    println("main: resolved, exiting");
    lean_runtime::sched::effect();
    ok(Handle::stdout().flush());
    crate::glue::process_exit(0)
}

// tests/cases/process/handoff_then_write.lean (AR-8: the second handle's
// lock waits for the first handle's writer, as natively its `fclose`)
fn handoff_then_write(args: &[String]) -> u32 {
    let (f, n) = (args[0].clone(), to_nat(&args[1]) as usize);
    let mk = ok(lio::spawn("mkfifo", &[&f], lio::INHERIT));
    let _ = mk.process.wait();
    // a program with tasks
    let _t = as_task(|| (), PRIO_DEFAULT);
    let script = format!("exec 3<{f}; sleep 1; cat <&3 > out; echo done > marker");
    let _child = ok(lio::spawn("sh", &["-c", &script], lio::INHERIT));
    let h1 = ok(Handle::open(f.as_bytes(), FsMode::Write));
    let h2 = ok(Handle::open(f.as_bytes(), FsMode::Write));
    ok(h1.put_str(&vec![b'X'; n]));
    {
        // the translator's free path
        let _scope = lean_runtime::sched::no_suspend();
        drop(h1);
    }
    ok(h2.put_str(b"Y"));
    ok(h2.flush());
    println("written");
    0
}

// tests/cases/process/handoff_then_kill.lean (AR-8: `kill` waits for the
// handed-off stream's writer, as natively the drop's `fclose` returned first)
fn handoff_then_kill(args: &[String]) -> u32 {
    let n = to_nat(&args[0]) as usize;
    // a program with tasks
    let _t = as_task(|| (), PRIO_DEFAULT);
    let child = ok(lio::spawn(
        "sh",
        &["-c", "sleep 1; cat > out; exec sleep 30"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Null,
            stderr: Stdio::Null,
        },
    ));
    let stdin = child.stdin.expect("piped");
    ok(stdin.put_str(&vec![b'a'; n]));
    {
        // the translator's free path
        let _scope = lean_runtime::sched::no_suspend();
        drop(stdin);
    }
    ok(child.process.kill());
    let code = ok(child.process.wait());
    println(&format!("killed, exit {code}"));
    0
}

// tests/cases/io/lock_during_read.lean
fn lock_during_read(args: &[String]) -> u32 {
    let h = ok(Handle::open(args[0].as_bytes(), FsMode::Read));
    let th = h.clone();
    let t = as_task(
        move || -> R<()> {
            let l = lio::get_line(&th)?;
            println(&format!("task: got {}", quote(&l)));
            Ok(())
        },
        PRIO_DEDICATED,
    );
    sleep(to_nat(&args[1]) as u32);
    ok(h.lock(true));
    println("main: locked while the task reads");
    let _ = t.get();
    ok(h.unlock());
    println("done");
    0
}

// ---------------------------------------------------------------------------
// tests/cases/uvloop: Std.Internal.UV's loop, timers and signals over the
// scheduler's event loop. A promise the program holds alone is released
// right after its last use, as compiled Lean releases it (the last release
// resolves it with `none`).

use crate::lio::{repr_int, repr_unit};
use lean_runtime::sched::uv::{self, Signal, Timer};

type UTimer = Timer<UvPromise<()>>;

/// A libuv error code as Lean's error (`lean_decode_uv_error`); the twins
/// expect none.
fn uv_ok<T>(r: Result<T, i32>) -> T {
    ok(r.map_err(|e| lean_runtime::io::IoError::decode_uv_error(e, None)))
}
type USignal = Signal<UvPromise<i64>>;

fn finished<T: Clone + 'static>(p: &UvPromise<T>) -> bool {
    has_finished(&p.result_opt())
}

// tests/cases/uvloop/loop_configure.lean
fn loop_configure(args: &[String]) -> u32 {
    println(&format!("alive: {}", uv::loop_alive()));
    uv_ok(uv::loop_configure(args[0] == "1", args[1] == "1"));
    println(&format!("configured, alive: {}", uv::loop_alive()));
    uv_ok(uv::loop_configure(false, false));
    println(&format!("alive: {}", uv::loop_alive()));
    0
}

// tests/cases/uvloop/get_tid_loop_thread.lean (review AR-37)
fn get_tid_loop_thread(_: &[String]) -> u32 {
    use lean_runtime::io::env::get_tid;
    let mt = get_tid();
    let a = as_task(get_tid, PRIO_DEFAULT).get();
    let on_loop = || {
        let t: UTimer = Timer::new(20, false);
        let p = t.next(UvPromise::new);
        let s = map_task(|_| get_tid(), p.result_opt(), PRIO_DEFAULT, true, true);
        drop(p);
        s.get()
    };
    let x = on_loop();
    sleep(50);
    let y = on_loop();
    println(&format!(
        "both timers' dependents on one thread: {}",
        x == y
    ));
    println(&format!("not main's: {}", x != mt));
    println(&format!("not the pool worker's: {}", x != a));
    0
}

// tests/cases/uvloop/loop_blocked_at_exit.lean (review HL2-02): `main`
// returns while the timer's `sync` dependent sleeps in a loop on the loop
// context, which the final run leaves suspended.
fn loop_blocked_at_exit(_: &[String]) -> u32 {
    let t: UTimer = Timer::new(10, false);
    let p = t.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            println("dependent runs on the loop");
            let _ = Handle::stdout().flush();
            loop {
                sleep(1000);
            }
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(t);
    sleep(1000);
    eprintln("main's stderr line");
    println("main done");
    3
}

// tests/cases/uvloop/loop_task_at_exit.lean (review RF13-01): the
// timer's `sync` dependent runs the IO task `t` on the loop context's stack
// (`IO.waitAny`), where `t` sleeps when `main` returns; the final run
// waits for it, as natively for the pool worker that runs it.
fn loop_task_at_exit(_: &[String]) -> u32 {
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            let t = as_task(
                || {
                    sleep(1000);
                    println("t done");
                    let _ = Handle::stdout().flush();
                },
                PRIO_DEFAULT,
            );
            wait_any(&[t]);
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    println("main done");
    0
}

// tests/cases/uvloop/loop_sleep_expired.lean (review RF13-02; the twin
// spins args[1] ms instead of the calibrated computation): the loop
// context's sleep ends while the final run runs the computing task on
// `main`'s stack.
fn loop_sleep_expired(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let len = Ref::new(ms);
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            sleep(500);
            println("late");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_sleep_poll_print.lean (review RF13-04; the twin
// spins args[1] ms instead of the calibrated computation): after the
// computing task, the loop context's callback reads the clock once, then
// prints; the final run lets it go on until it ends.
fn loop_sleep_poll_print(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let len = Ref::new(ms);
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            sleep(300);
            let _ = mono_ms_now();
            println("late");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_sleep_compute_print.lean (review RF13-04; the
// twin spins args[1] ms in the task and 10 ms in the dependent instead of
// the calibrated computations): after the computing task, the loop
// context's callback computes, then prints; its print's effect point lets
// `main` go first, and the final run lets it go on until it ends.
fn loop_sleep_compute_print(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let len = Ref::new(ms);
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |r: Option<()>| {
            sleep(500);
            spin_ms(10 + u64::from(r.is_none()));
            println("late true");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_valve_counts_run.lean (review RF13-07; the twin
// spins args[1] and args[2] ms instead of the calibrated computations):
// after `main`'s computing task, the loop context's callback starts a
// computing task, reads the clock, then prints; the time that task runs on
// `main`'s stack does not count against the loop context's valve.
fn loop_valve_counts_run(args: &[String]) -> u32 {
    let main_len = Ref::new(to_nat(&args[1]));
    let dep_len = Ref::new(to_nat(&args[2]));
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        move |_: Option<()>| {
            sleep(300);
            let _t = as_task(move || spin_ms(dep_len.get()), PRIO_DEFAULT);
            let _ = mono_ms_now();
            println("late");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(main_len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_spawn_poll.lean (fixes-13, round 4; the twin
// spins args[1] and args[2] ms instead of the calibrated computations):
// the loop context's callback starts a computing task, reads the clock and
// prints at once; the final run runs the task once the callback has ended.
fn loop_spawn_poll(args: &[String]) -> u32 {
    let main_len = Ref::new(to_nat(&args[1]));
    let dep_len = Ref::new(to_nat(&args[2]));
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        move |_: Option<()>| {
            sleep(300);
            let _t = as_task(
                move || {
                    spin_ms(dep_len.get());
                    println("task done");
                },
                PRIO_DEFAULT,
            );
            let _ = mono_ms_now();
            println("callback done");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(main_len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_poll_work.lean (fixes-13, round 4; the twin's
// task spins args[1] ms instead of the calibrated computation; the
// callback's updates are calibrated as in the program, 25 times the
// updates per 100 ms instead of 15, so that a 1 s limit surely cuts it):
// the callback, longer than 1 s and with scheduling points, goes on after
// `main`'s task for as long as that task took, plus 1 s.
fn loop_poll_work(args: &[String]) -> u32 {
    let r = Ref::new(0u64);
    let rw = r.clone();
    as_task(move || rw.modify(|v| v), PRIO_DEFAULT).get();
    let t0 = std::time::Instant::now();
    let mut updates = 0u64;
    while t0.elapsed() < std::time::Duration::from_millis(100) {
        for _ in 0..10000 {
            r.modify(|v| v + 1);
        }
        updates += 10000;
    }
    r.set(0);
    let k = 25 * updates;
    let len = Ref::new(to_nat(&args[1]));
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let r2 = r.clone();
    let _dep = map_task(
        move |_: Option<()>| {
            for _ in 0..k {
                r2.modify(|v| v + 1);
            }
            println(&format!("late {}", r2.get() == k));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_spins_on_task.lean (review RF13-10; the twin
// spins args[1] ms instead of the calibrated computation): the loop
// context's callback starts a task that sets a flag and spins on it; the
// task, queued for `STALE`, runs on `main`'s stack, and the callback goes
// on.
fn loop_spins_on_task(args: &[String]) -> u32 {
    let len = Ref::new(to_nat(&args[1]));
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            sleep(300);
            let flag = Ref::new(false);
            let f2 = flag.clone();
            let _t = as_task(move || f2.set(true), PRIO_DEFAULT);
            while !flag.get() {
                let _ = mono_ms_now();
            }
            let _ = mono_ms_now();
            println("late");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_cycle.lean (fixes-13, round 5): the loop
// context's callback cycles forever, a task it waits for (run on its stack,
// and waited for as a worker's), then 300 ms of reference updates; the
// final run counts its time alone over the whole run, so the exit comes
// about 1 s after `main` returns.
fn loop_cycle(_: &[String]) -> u32 {
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let r = Ref::new(0u64);
    let _dep = map_task(
        move |_: Option<()>| {
            println("cycling");
            let _ = Handle::stdout().flush();
            loop {
                let t = as_task(|| sleep(1), PRIO_DEFAULT);
                wait_any(&[t]);
                let t0 = mono_ms_now();
                while mono_ms_now() - t0 < 300 {
                    r.modify(|v| v + 1);
                }
            }
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    println("main done");
    let _ = Handle::stdout().flush();
    0
}

// tests/cases/uvloop/loop_sleeps_after_task.lean (review RF13-12; the twin
// spins args[1] ms instead of the calibrated computation): after `main`'s
// task, the loop context's callback prints, sleeps 100 ms and prints; the
// final run waits for that sleep, which ends within the task's time.
fn loop_sleeps_after_task(args: &[String]) -> u32 {
    let len = Ref::new(to_nat(&args[1]));
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            sleep(300);
            println("a");
            sleep(100);
            println("b");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/loop_cycle_long.lean (review RF13-13): the loop
// context's callback waits for a 400 ms task in each of 5 cycles, then
// polls 300 ms; the final run counts the waits for nothing, so the polling
// uses up the loop context's 1 s during the fourth cycle.
fn loop_cycle_long(_: &[String]) -> u32 {
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let r = Ref::new(0u64);
    let _dep = map_task(
        move |_: Option<()>| {
            println("cycling");
            let _ = Handle::stdout().flush();
            for _ in 0..5 {
                let t = as_task(|| sleep(400), PRIO_DEFAULT);
                wait_any(&[t]);
                let t0 = mono_ms_now();
                while mono_ms_now() - t0 < 300 {
                    r.modify(|v| v + 1);
                }
            }
            println("dep done");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    println("main done");
    0
}

// tests/cases/uvloop/loop_carried_then_poll.lean (review RF13-14): the loop
// context's callback waits for a 1.2 s task it carries, then reads the
// clock and prints; the wait costs the loop context nothing.
fn loop_carried_then_poll(_: &[String]) -> u32 {
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            let t = as_task(|| sleep(1200), PRIO_DEFAULT);
            wait_any(&[t]);
            let _ = mono_ms_now();
            println("after");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(100);
    println("main done");
    0
}

// tests/cases/uvloop/loop_polls_at_exit.lean (review RF13-03): the loop
// context reads the clock forever when `main` returns.
fn loop_polls_at_exit(_: &[String]) -> u32 {
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            println("dependent polls the clock");
            let _ = Handle::stdout().flush();
            loop {
                mono_ms_now();
            }
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(300);
    println("main done");
    let _ = Handle::stdout().flush();
    0
}

// tests/cases/uvloop/timer_oneshot.lean
fn timer_oneshot(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let t: UTimer = Timer::new(ms, false);
    let t0 = mono_ms_now();
    let p = t.next(UvPromise::new);
    println(&format!("pending after next: {}", !finished(&p)));
    let r = p.result_opt().get();
    drop(p);
    let dt = mono_ms_now() - t0;
    println(&format!(
        "resolved {}, after at least {ms} ms: {}",
        repr_unit(&r),
        dt >= ms
    ));
    let p2 = t.next(UvPromise::new);
    println(&format!("second next resolved at once: {}", finished(&p2)));
    drop(p2);
    t.reset();
    t.cancel();
    let p3 = t.next(UvPromise::new);
    println(&format!(
        "after reset and cancel, next resolved: {}",
        finished(&p3)
    ));
    drop(p3);
    t.stop();
    let p4 = t.next(UvPromise::new);
    sleep((2 * ms) as u32);
    println(&format!("after stop, next resolved: {}", finished(&p4)));
    p4.resolve(());
    0
}

// tests/cases/uvloop/timer_repeating.lean
fn timer_repeating(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let n = to_nat(&args[1]);
    let t: UTimer = Timer::new(ms, true);
    let t0 = mono_ms_now();
    let p = t.next(UvPromise::new);
    let r = p.result_opt().get();
    drop(p);
    println(&format!("first tick {}", repr_unit(&r)));
    for i in 0..n {
        let a = t.next(UvPromise::new);
        let b = t.next(UvPromise::new);
        let fa = finished(&a);
        let _ = b.result_opt().get();
        drop(b);
        println(&format!(
            "tick {}: pending before {}, same promise resolved: {}",
            i + 1,
            !fa,
            finished(&a)
        ));
    }
    let dt = mono_ms_now() - t0;
    println(&format!(
        "{n} periods took at least {} ms: {}",
        n * ms,
        dt >= n * ms
    ));
    t.stop();
    let q = t.next(UvPromise::new);
    sleep((2 * ms) as u32);
    println(&format!("after stop, next resolved: {}", finished(&q)));
    q.resolve(());
    drop(q);
    let z: UTimer = Timer::new(0, true);
    let z0 = z.next(UvPromise::new);
    let _ = z0.result_opt().get();
    drop(z0);
    let z1 = z.next(UvPromise::new);
    sleep((3 * ms) as u32);
    println(&format!(
        "timeout 0: first resolved, second resolved: {}",
        finished(&z1)
    ));
    drop(z1);
    z.stop();
    0
}

// tests/cases/uvloop/timer_cancel_reset.lean
fn timer_cancel_reset(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let t: UTimer = Timer::new(ms, false);
    let p = t.next(UvPromise::new);
    t.cancel();
    sleep((2 * ms) as u32);
    println(&format!(
        "one-shot: cancelled promise resolved: {}",
        finished(&p)
    ));
    p.resolve(());
    drop(p);
    let p2 = t.next(UvPromise::new);
    let r = p2.result_opt().get();
    drop(p2);
    println(&format!(
        "one-shot: next after cancel resolves {}",
        repr_unit(&r)
    ));
    let u: UTimer = Timer::new(ms, true);
    let u0 = u.next(UvPromise::new);
    let _ = u0.result_opt().get();
    drop(u0);
    let u1 = u.next(UvPromise::new);
    u.cancel();
    sleep((2 * ms) as u32);
    println(&format!(
        "repeating: cancelled promise resolved: {}",
        finished(&u1)
    ));
    u1.resolve(());
    drop(u1);
    let u2 = u.next(UvPromise::new);
    let r = u2.result_opt().get();
    drop(u2);
    println(&format!(
        "repeating: next after cancel resolves {}",
        repr_unit(&r)
    ));
    u.stop();
    let v: UTimer = Timer::new(2 * ms, false);
    let t0 = mono_ms_now();
    let q = v.next(UvPromise::new);
    sleep(ms as u32);
    v.reset();
    sleep(ms as u32);
    println(&format!(
        "reset: resolved before the moved deadline: {}",
        finished(&q)
    ));
    let r = q.result_opt().get();
    drop(q);
    let dt = mono_ms_now() - t0;
    println(&format!(
        "reset: resolves {} after at least {} ms: {}",
        repr_unit(&r),
        3 * ms,
        dt >= 3 * ms
    ));
    0
}

/// `kill (sig : String)`: `IO.Process.output` of `kill -SIG <pid>`.
fn kill_self(sig: &str) {
    let pid = lio::get_pid().to_string();
    let _ = ok(lio::output("kill", &[&format!("-{sig}"), &pid]));
}

// tests/cases/uvloop/signal_usr1.lean
fn signal_usr1(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let s: USignal = Signal::new(num as i32, false);
    let p = uv_ok(s.next(UvPromise::new));
    println(&format!("one-shot pending: {}", !finished(&p)));
    kill_self("USR1");
    let r = p.result_opt().get();
    drop(p);
    println(&format!("one-shot got {}", repr_int(&r)));
    let p2 = uv_ok(s.next(UvPromise::new));
    println(&format!("one-shot second next resolved: {}", finished(&p2)));
    drop(p2);
    let m: USignal = Signal::new(num as i32, true);
    for i in 0..2 {
        let q = uv_ok(m.next(UvPromise::new));
        kill_self("USR1");
        let r = q.result_opt().get();
        drop(q);
        println(&format!("repeating {i}: got {}", repr_int(&r)));
    }
    let q = uv_ok(m.next(UvPromise::new));
    m.cancel();
    kill_self("USR1");
    sleep(100);
    println(&format!(
        "repeating: cancelled promise resolved: {}",
        finished(&q)
    ));
    q.resolve(0);
    drop(q);
    let q2 = uv_ok(m.next(UvPromise::new));
    kill_self("USR1");
    let r = q2.result_opt().get();
    drop(q2);
    println(&format!("repeating: after cancel got {}", repr_int(&r)));
    match USignal::new(99, false).next(UvPromise::new) {
        Ok(_) => println("signal 99: next succeeded"),
        Err(e) => println(&format!(
            "signal 99: {}",
            lio::error_text(&lean_runtime::io::IoError::decode_uv_error(e, None))
        )),
    }
    uv_ok(m.stop());
    uv_ok(s.stop());
    println("stopped; the next SIGUSR1 ends the program");
    let _ = Handle::stdout().flush();
    kill_self("USR1");
    sleep(1000);
    println("not reached");
    0
}

/// `run cmd`: `IO.Process.output { cmd }`.
fn run_cmd(cmd: &str) {
    let _ = ok(lio::output(cmd, &[]));
}

// tests/cases/uvloop/signal_stale.lean
fn signal_stale(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let w: USignal = Signal::new(num as i32, true);
    let p = uv_ok(w.next(UvPromise::new));
    run_cmd("true");
    let r = p.result_opt().get();
    drop(p);
    println(&format!("first watcher got {}", repr_int(&r)));
    uv_ok(w.stop());
    run_cmd("true");
    println("a child exited with no watcher");
    let w2: USignal = Signal::new(num as i32, false);
    let q = uv_ok(w2.next(UvPromise::new));
    sleep(200);
    println(&format!(
        "the new watcher saw the earlier signal: {}",
        finished(&q)
    ));
    run_cmd("true");
    let r = q.result_opt().get();
    drop(q);
    println(&format!("the new watcher got {}", repr_int(&r)));
    0
}

// tests/cases/uvloop/signal_oneshot_twice.lean
fn signal_oneshot_twice(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let s: USignal = Signal::new(num as i32, false);
    let p = uv_ok(s.next(UvPromise::new));
    let pid = lio::get_pid();
    // a process spawn is an effect point
    lean_runtime::sched::effect();
    let _child = ok(lio::spawn(
        "sh",
        &[
            "-c",
            &format!("kill -USR1 {pid}; sleep 0.05; kill -USR1 {pid}"),
        ],
        lio::INHERIT,
    ));
    println("before");
    let _ = Handle::stdout().flush();
    // `spin`: seconds of pure computation, never a yield point
    let n = to_nat(&args[1]);
    let mut x = (pid as u64) | 1;
    for _ in 0..n {
        x = std::hint::black_box(
            x.wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        );
    }
    println(&format!("not reached {x} {}", finished(&p)));
    0
}

// tests/cases/uvloop/signal_failed_next.lean (LB-19: the correct outcome)
fn signal_failed_next(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let s: USignal = Signal::new(num as i32, false);
    let text = |e: i32| lio::error_text(&lean_runtime::io::IoError::decode_uv_error(e, None));
    match s.next(UvPromise::new) {
        Ok(_) => println("first next: ok"),
        Err(e) => println(&format!("first next: {}", text(e))),
    }
    s.cancel();
    println("cancelled");
    match s.next(UvPromise::new) {
        Ok(_) => println("second next: ok"),
        Err(e) => println(&format!("second next: {}", text(e))),
    }
    uv_ok(s.stop());
    println("done");
    0
}

// ---------------------------------------------------------------------------
// The cases of the sched-io Part B review (RSIOB) and LB-20.

/// A pure computation of `ms` milliseconds, with no scheduler call: the
/// twin's stand-in for the native case's busy loop (whose length, the case's
/// first argument, is about the same duration natively).
fn spin_ms(ms: u64) {
    let t = std::time::Instant::now();
    let mut acc = 0u64;
    while t.elapsed() < std::time::Duration::from_millis(ms) {
        acc = std::hint::black_box(acc.wrapping_mul(31).wrapping_add(7));
    }
    std::hint::black_box(acc);
}

/// `let _ ← IO.Process.spawn { cmd := "sh", args := #["-c", script] }`.
fn spawn_sh(script: &str) {
    lean_runtime::sched::effect();
    let c = ok(lio::spawn("sh", &["-c", script], lio::INHERIT));
    drop(c);
}

// tests/cases/uvloop/signal_stale_deferred.lean
fn signal_stale_deferred(_: &[String]) -> u32 {
    let w: USignal = Signal::new(17, true);
    let p = uv_ok(w.next(UvPromise::new));
    run_cmd("true");
    let r = p.result_opt().get();
    drop(p);
    println(&format!("first watcher got {}", repr_int(&r)));
    uv_ok(w.stop());
    run_cmd("true");
    println("a child exited with no SIGCHLD watcher");
    let u: USignal = Signal::new(10, false);
    let pu = uv_ok(u.next(UvPromise::new));
    let w2: USignal = Signal::new(17, false);
    let q = uv_ok(w2.next(UvPromise::new));
    sleep(200);
    println(&format!(
        "the new SIGCHLD watcher saw the earlier signal: {}",
        finished(&q)
    ));
    q.resolve(0);
    pu.resolve(0);
    uv_ok(u.stop());
    uv_ok(w2.stop());
    0
}

// tests/cases/uvloop/timer_due_stop.lean (the twin spins args[1] ms)
fn timer_due_stop(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let t: UTimer = Timer::new(10, false);
    let p = t.next(UvPromise::new);
    spin_ms(ms);
    t.stop();
    let task = p.result_opt();
    drop(p);
    let r = task.get();
    println(&format!("due timer, then stop: {}", repr_unit(&r)));
    0
}

// tests/cases/uvloop/signal_cancel_restart.lean (the twin spins args[1] ms)
fn signal_cancel_restart(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let s: USignal = Signal::new(10, false);
    drop(uv_ok(s.next(UvPromise::new)));
    let pid = lio::get_pid();
    spawn_sh(&format!("sleep 0.3; kill -USR1 {pid}"));
    spin_ms(ms);
    s.cancel();
    let p2 = uv_ok(s.next(UvPromise::new));
    sleep(100);
    println(&format!(
        "after cancel and next, resolved: {}",
        finished(&p2)
    ));
    p2.resolve(0);
    uv_ok(s.stop());
    0
}

// tests/cases/uvloop/signal_order.lean
fn signal_order(_: &[String]) -> u32 {
    let a: USignal = Signal::new(10, false);
    let b: USignal = Signal::new(10, true);
    let pa = uv_ok(a.next(UvPromise::new));
    let pb = uv_ok(b.next(UvPromise::new));
    let ta = map_task(
        |_: Option<i64>| println("one-shot (started first)"),
        pa.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let tb = map_task(
        |_: Option<i64>| println("repeating (started second)"),
        pb.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(pa);
    drop(pb);
    kill_self("USR1");
    ta.get();
    tb.get();
    uv_ok(b.stop());
    0
}

/// `(← System.FilePath.readDir "/proc/self/fd").size`.
fn fd_count() -> u64 {
    let mut n = 0;
    ok(lean_runtime::io::fs::read_dir(b"/proc/self/fd", |_| n += 1));
    n
}

// tests/cases/uvloop/signal_fds.lean
fn signal_fds(_: &[String]) -> u32 {
    let before = fd_count();
    let s: USignal = Signal::new(10, true);
    let p = uv_ok(s.next(UvPromise::new));
    let after = fd_count();
    println(&format!(
        "descriptors added by the first watcher: {}",
        after - before
    ));
    p.resolve(0);
    drop(p);
    uv_ok(s.stop());
    println(&format!("after stop: {}", fd_count() - before));
    0
}

// tests/cases/uvloop/exit_listening.lean
fn exit_listening(_: &[String]) -> u32 {
    let s: USignal = Signal::new(10, true);
    let p = uv_ok(s.next(UvPromise::new));
    let _a = map_task(
        |r: Option<i64>| eprintln(&format!("signal dependent ran: {}", repr_int(&r))),
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(s);
    let t: UTimer = Timer::new(100000, true);
    let q = t.next(UvPromise::new);
    let _ = q.result_opt().get();
    drop(q);
    let q2 = t.next(UvPromise::new);
    let _b = map_task(
        |r: Option<()>| eprintln(&format!("timer dependent ran: {}", repr_unit(&r))),
        q2.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(q2);
    drop(t);
    println("main returns");
    0
}

// tests/cases/uvloop/signal_sigio_default.lean
fn signal_sigio_default(_: &[String]) -> u32 {
    let s: USignal = Signal::new(29, true);
    drop(uv_ok(s.next(UvPromise::new)));
    uv_ok(s.stop());
    kill_self("IO");
    sleep(200);
    println("survived SIGIO");
    0
}

// tests/cases/uvloop/{timer,signal}_{stop,cancel}_in_sync_dependent.lean
// (the judge's LB20_Probe.lean; args: KIND OP MODE AFTER)
fn lb20_probe(args: &[String]) -> u32 {
    let say = |s: &str| {
        println(s);
        let _ = Handle::stdout().flush();
    };
    let (kind, stop) = (args[0].as_str(), args[1] == "stop");
    let sync = args[2] == "sync";
    let after = args[3].as_str();
    if kind == "timer" {
        let t: UTimer = Timer::new(10, false);
        let p = t.next(UvPromise::new);
        let t2 = t.clone();
        let tk = map_task(
            move |_: Option<()>| {
                if stop {
                    t2.stop()
                } else {
                    t2.cancel()
                }
            },
            p.result_opt(),
            PRIO_DEFAULT,
            sync,
            true,
        );
        tk.get();
        say("dependent ran: ok");
        say(&format!("first promise resolved: {}", finished(&p)));
        drop(p);
        match after {
            "next" => {
                for _ in 0..3 {
                    let q = t.next(UvPromise::new);
                    let task = q.result_opt();
                    drop(q);
                    say(&format!("next: {}", has_finished(&task)));
                }
            }
            "reset" => {
                t.reset();
                say("reset: ok");
            }
            _ => {}
        }
    } else {
        let s: USignal = Signal::new(10, false);
        let p = uv_ok(s.next(UvPromise::new));
        let s2 = s.clone();
        let tk = map_task(
            move |_: Option<i64>| {
                if stop {
                    let _ = s2.stop();
                } else {
                    s2.cancel();
                }
            },
            p.result_opt(),
            PRIO_DEFAULT,
            sync,
            true,
        );
        kill_self("USR1");
        tk.get();
        say("dependent ran: ok");
        say(&format!("first promise resolved: {}", finished(&p)));
        drop(p);
        if after == "next" {
            for _ in 0..3 {
                let q = uv_ok(s.next(UvPromise::new));
                let task = q.result_opt();
                drop(q);
                say(&format!("next: {}", has_finished(&task)));
            }
        }
    }
    say("done");
    0
}

// tests/cases/uvloop/timer_catchup_bound.lean: `arm` re-subscribes from a
// `sync` dependent of each tick, on `some` only (LB-33: at `t.stop` the
// dependent reads `none` once and returns).
fn catchup_arm(t: UTimer, n: Ref<u64>, work_ms: u64) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            spin_ms(work_ms);
            n.modify(|k| k + 1);
            if v.is_some() {
                catchup_arm(t, n, work_ms);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
        true,
    );
}

fn timer_catchup_bound(args: &[String]) -> u32 {
    let work_ms = to_nat(&args[1]);
    let n = Ref::new(0u64);
    let t: UTimer = Timer::new(1, true);
    catchup_arm(t.clone(), n.clone(), work_ms);
    sleep(100);
    let t0 = std::time::Instant::now();
    let u: UTimer = Timer::new(1000, false);
    let dt = t0.elapsed().as_millis();
    let k = n.get();
    println(&format!(
        "Timer.mk under 2 s: {}; under 500 ticks so far: {}",
        dt < 2000,
        k < 500
    ));
    t.stop();
    drop(u);
    0
}

// The cases of LB-33 and LB-34 (tests/cases/uvloop/*_rearm_*, *_keep_*,
// *_resubscribe, signal_stop_drops_promise): `say` is `IO.println` and a
// flush.
fn say(s: &str) {
    println(s);
    let _ = Handle::stdout().flush();
}

fn some_none<T>(v: &Option<T>) -> &'static str {
    if v.is_some() {
        "some"
    } else {
        "none"
    }
}

/// `showV` of the signal cases.
fn show_v(v: &Option<i64>) -> String {
    match v {
        Some(s) => format!("some {s}"),
        None => "none".into(),
    }
}

/// Lean's `toString` of an `Array String`.
fn show_array(a: &[String]) -> String {
    format!("#[{}]", a.join(", "))
}

/// `arm` of the timer cases: a `sync` dependent that prints its value and
/// subscribes again on any value, at most `cap` times.
fn timer_arm_capped(t: UTimer, n: Ref<u64>, cap: u64) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            n.modify(|k| k + 1);
            let k = n.get();
            say(&format!("dependent {k}: value {}", some_none(&v)));
            if k < cap {
                timer_arm_capped(t, n, cap);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
        true,
    );
}

/// `arm` of the timer cases that record values: a dependent (`sync` or not)
/// that records its value and subscribes again while it has fewer than
/// `cap` values, then resolves `done`.
fn timer_arm_record(
    t: UTimer,
    values: Ref<Vec<String>>,
    cap: usize,
    done: UvPromise<()>,
    sync: bool,
) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            values.modify(|mut a| {
                a.push(some_none(&v).into());
                a
            });
            if values.get().len() < cap {
                timer_arm_record(t, values, cap, done, sync);
            } else {
                done.resolve(());
            }
        },
        task,
        PRIO_DEFAULT,
        sync,
        true,
    );
}

// tests/cases/uvloop/timer_stop_rearm_in_sync_dependent.lean (LB-33)
fn timer_stop_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let (period, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let n = Ref::new(0u64);
    let t: UTimer = Timer::new(period, true);
    let p0 = t.next(UvPromise::new);
    let r = p0.result_opt().get();
    drop(p0);
    say(&format!("0th tick: {}", some_none(&r)));
    timer_arm_capped(t.clone(), n.clone(), cap);
    say("stop: begin");
    t.stop();
    say("stop: end");
    sleep(100);
    say(&format!("dependent runs: {}", n.get()));
    say("main: end");
    0
}

// tests/cases/uvloop/timer_cancel_rearm_in_sync_dependent.lean (LB-33):
// the dependent subscribes again in its first run only, and its second
// run resolves `done`.
fn timer_cancel_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let period = to_nat(&args[0]);
    let values = Ref::new(Vec::<String>::new());
    let done: UvPromise<()> = UvPromise::new();
    let t: UTimer = Timer::new(period, true);
    let p0 = t.next(UvPromise::new);
    let r = p0.result_opt().get();
    drop(p0);
    say(&format!("0th tick: {}", some_none(&r)));
    timer_arm_record(t.clone(), values.clone(), 2, done.clone(), true);
    say("cancel: begin");
    t.cancel();
    say("cancel: end");
    let _ = done.result_opt().get();
    say(&format!("dependent values: {}", show_array(&values.get())));
    t.stop();
    say("stop: end");
    0
}

// tests/cases/uvloop/timer_oneshot_{stop,cancel}_keep_in_sync_dependent.lean
// (LB-33)
fn timer_oneshot_keep_in_sync_dependent(args: &[String]) -> u32 {
    let op = args[0].clone();
    let (ms, count) = (to_nat(&args[1]), to_nat(&args[2]));
    let kept: Ref<Option<UvPromise<()>>> = Ref::new(None);
    let t: UTimer = Timer::new(ms, false);
    let p = t.next(UvPromise::new);
    let (t2, kept2) = (t.clone(), kept.clone());
    let _ = map_task(
        move |v: Option<()>| {
            let q = t2.next(UvPromise::new);
            say(&format!("dependent: value {}", some_none(&v)));
            kept2.set(Some(q));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    say(&format!("{op}: begin"));
    if op == "cancel" {
        t.cancel()
    } else {
        t.stop()
    }
    say(&format!("{op}: end"));
    let fresh: Vec<UvPromise<()>> = (0..count).map(|_| UvPromise::new()).collect();
    let Some(q) = kept.get() else {
        say("nothing kept");
        crate::glue::process_exit(0)
    };
    let a = q.addr();
    say(&format!("aliased: {}", fresh.iter().any(|f| f.addr() == a)));
    say(&format!("kept resolved: {}", finished(&q)));
    for f in &fresh {
        f.resolve(());
    }
    say(&format!(
        "kept resolved after resolving the fresh promises: {}",
        finished(&q)
    ));
    crate::glue::process_exit(0)
}

// tests/cases/uvloop/timer_oneshot_cancel_resubscribe.lean (LB-33)
fn timer_oneshot_cancel_resubscribe(args: &[String]) -> u32 {
    let (ms, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let n = Ref::new(0u64);
    let t: UTimer = Timer::new(ms, false);
    timer_arm_capped(t.clone(), n.clone(), cap);
    say("cancel: begin");
    t.cancel();
    say("cancel: end");
    sleep(100);
    say(&format!("dependent runs: {}", n.get()));
    say("stop: begin");
    t.stop();
    say("stop: end");
    0
}

// tests/cases/uvloop/timer_oneshot_stop_resubscribe.lean (a control of
// LB-33; review RF2-L-01)
fn timer_oneshot_stop_resubscribe(args: &[String]) -> u32 {
    let (ms, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let n = Ref::new(0u64);
    let t: UTimer = Timer::new(ms, false);
    timer_arm_capped(t.clone(), n.clone(), cap);
    say("stop: begin");
    t.stop();
    say("stop: end");
    sleep(100);
    say(&format!("dependent runs: {}", n.get()));
    0
}

// tests/cases/uvloop/timer_stop_rearm_async_dependent.lean (a control of
// LB-33)
fn timer_stop_rearm_async_dependent(args: &[String]) -> u32 {
    let (period, cap) = (to_nat(&args[0]), to_nat(&args[1]));
    let values = Ref::new(Vec::<String>::new());
    let done: UvPromise<()> = UvPromise::new();
    let t: UTimer = Timer::new(period, true);
    let p0 = t.next(UvPromise::new);
    let r = p0.result_opt().get();
    drop(p0);
    say(&format!("0th tick: {}", some_none(&r)));
    timer_arm_record(t.clone(), values.clone(), cap as usize, done.clone(), false);
    t.stop();
    say("stop: end");
    let _ = done.result_opt().get();
    say(&format!("dependent values: {}", show_array(&values.get())));
    0
}

/// `arm` of the signal cases: as `timer_arm_capped`.
fn signal_arm_capped(s: USignal, n: Ref<u64>, cap: u64) {
    let p = uv_ok(s.next(UvPromise::new));
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<i64>| {
            n.modify(|k| k + 1);
            let k = n.get();
            say(&format!("dependent {k}: value {}", some_none(&v)));
            if k < cap {
                signal_arm_capped(s, n, cap);
            }
        },
        task,
        PRIO_DEFAULT,
        true,
        true,
    );
}

// tests/cases/uvloop/signal_stop_rearm_in_sync_dependent.lean (LB-34)
fn signal_stop_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let cap = to_nat(&args[1]);
    let n = Ref::new(0u64);
    let s: USignal = Signal::new(num as i32, true);
    signal_arm_capped(s.clone(), n.clone(), cap);
    say("stop: begin");
    uv_ok(s.stop());
    say("stop: end");
    say(&format!("dependent runs: {}", n.get()));
    0
}

/// `arm` of `signal_cancel_rearm_in_sync_dependent`: records each value,
/// subscribes again in the first run only, and resolves `done` in the
/// second.
fn signal_arm_record(s: USignal, values: Ref<Vec<String>>, done: UvPromise<()>) {
    let p = uv_ok(s.next(UvPromise::new));
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<i64>| {
            values.modify(|mut a| {
                a.push(show_v(&v));
                a
            });
            if values.get().len() == 1 {
                signal_arm_record(s, values, done);
            } else {
                done.resolve(());
            }
        },
        task,
        PRIO_DEFAULT,
        true,
        true,
    );
}

// tests/cases/uvloop/signal_cancel_rearm_in_sync_dependent.lean (LB-34):
// the dependent's second run resolves `done`, which `main` waits for before
// it reads the values (review AR-41).
fn signal_cancel_rearm_in_sync_dependent(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let values = Ref::new(Vec::<String>::new());
    let done: UvPromise<()> = UvPromise::new();
    let s: USignal = Signal::new(num as i32, true);
    signal_arm_record(s.clone(), values.clone(), done.clone());
    say("cancel: begin");
    s.cancel();
    say("cancel: end");
    let q = uv_ok(s.next(UvPromise::new));
    kill_self("USR1");
    let r = q.result_opt().get();
    drop(q);
    say(&format!("main's next after cancel: {}", show_v(&r)));
    let _ = done.result_opt().get();
    say(&format!("dependent values: {}", show_array(&values.get())));
    uv_ok(s.stop());
    say("stop: end");
    0
}

// tests/cases/uvloop/signal_oneshot_{stop,cancel}_keep_in_sync_dependent.lean
// (LB-34)
fn signal_oneshot_keep_in_sync_dependent(args: &[String]) -> u32 {
    let op = args[0].clone();
    let num: i64 = args[1].parse().expect("an Int");
    let count = to_nat(&args[2]);
    let kept: Ref<Option<UvPromise<i64>>> = Ref::new(None);
    let s: USignal = Signal::new(num as i32, false);
    let p = uv_ok(s.next(UvPromise::new));
    let (s2, kept2) = (s.clone(), kept.clone());
    let _ = map_task(
        move |v: Option<i64>| {
            let q = uv_ok(s2.next(UvPromise::new));
            say(&format!("dependent: value {}", some_none(&v)));
            kept2.set(Some(q));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    say(&format!("{op}: begin"));
    if op == "cancel" {
        s.cancel()
    } else {
        uv_ok(s.stop())
    }
    say(&format!("{op}: end"));
    let fresh: Vec<UvPromise<i64>> = (0..count).map(|_| UvPromise::new()).collect();
    let Some(q) = kept.get() else {
        say("nothing kept");
        crate::glue::process_exit(0)
    };
    let a = q.addr();
    say(&format!("aliased: {}", fresh.iter().any(|f| f.addr() == a)));
    say(&format!("kept resolved: {}", finished(&q)));
    for f in &fresh {
        f.resolve(7);
    }
    say(&format!(
        "kept resolved after resolving the fresh promises: {}",
        finished(&q)
    ));
    crate::glue::process_exit(0)
}

// tests/cases/uvloop/signal_stop_drops_promise.lean (a control of LB-34)
fn signal_stop_drops_promise(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let n = Ref::new(0u64);
    let s: USignal = Signal::new(num as i32, true);
    let p = uv_ok(s.next(UvPromise::new));
    let n2 = n.clone();
    let _ = map_task(
        move |v: Option<i64>| {
            n2.modify(|k| k + 1);
            say(&format!("dependent {}: value {}", n2.get(), some_none(&v)));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    say("stop: begin");
    uv_ok(s.stop());
    say("stop: end");
    say(&format!("dependent runs: {}", n.get()));
    0
}

// tests/cases/uvloop/signal_rearm_in_{sync,async}_dependent.lean
fn signal_rearm_in_dependent(args: &[String]) -> u32 {
    let sync = args[0] == "sync";
    let a: USignal = Signal::new(10, false);
    let pa = uv_ok(a.next(UvPromise::new));
    let tb = map_task(
        move |_: Option<i64>| {
            let b: USignal = Signal::new(10, false);
            let pb = uv_ok(b.next(UvPromise::new));
            (b, pb)
        },
        pa.result_opt(),
        PRIO_DEFAULT,
        sync,
        true,
    );
    drop(pa);
    kill_self("USR1");
    let (_b, pb) = tb.get();
    println("B listening");
    let _ = Handle::stdout().flush();
    kill_self("USR1");
    let t = pb.result_opt();
    drop(pb);
    println(&format!("B got {}", repr_int(&t.get())));
    0
}

// tests/cases/uvloop/signal_reset_*_dependent.lean (review AR-50): argv is
// sync or async, the signal's Lean number and its name for `kill`
fn signal_reset_in_dependent(args: &[String]) -> u32 {
    let sync = args[0] == "sync";
    let num: i32 = args[1].parse().expect("an Int");
    let name = args[2].clone();
    let a: USignal = Signal::new(num, false);
    let pa = uv_ok(a.next(UvPromise::new));
    let tb = map_task(
        move |_: Option<i64>| {
            let b: USignal = Signal::new(num, false);
            let pb = uv_ok(b.next(UvPromise::new));
            (b, pb)
        },
        pa.result_opt(),
        PRIO_DEFAULT,
        sync,
        true,
    );
    drop(pa);
    kill_self(&name);
    let (b, pb) = tb.get();
    println("B listening");
    let _ = Handle::stdout().flush();
    kill_self(&name);
    sleep(300);
    println(&format!("B got: {}", finished(&pb)));
    let _ = b.stop();
    0
}

// tests/cases/uvloop/signal_reset_*_after_repeating_stop.lean (AR-50, part
// 2): argv is the signal's Lean number, its name for `kill`, the native busy
// loop's length and the port's spin in ms
fn signal_reset_after_repeating_stop(args: &[String]) -> u32 {
    let num: i32 = args[0].parse().expect("an Int");
    let name = args[1].clone();
    let ms = to_nat(&args[3]);
    let pid = lio::get_pid();
    let w: USignal = Signal::new(num, true);
    let pw = uv_ok(w.next(UvPromise::new));
    let o: USignal = Signal::new(num, false);
    let w2 = w.clone();
    let kill_name = name.clone();
    let tt = map_task(
        move |_: Option<i64>| {
            let po = uv_ok(o.next(UvPromise::new));
            let tb = map_task(
                move |_: Option<i64>| {
                    let b: USignal = Signal::new(num, false);
                    let pb = uv_ok(b.next(UvPromise::new));
                    (b, pb)
                },
                po.result_opt(),
                PRIO_DEFAULT,
                true,
                true,
            );
            drop(po);
            spawn_sh(&format!("sleep 0.1; kill -{kill_name} {pid}"));
            spin_ms(ms);
            uv_ok(w2.stop());
            tb
        },
        pw.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(pw);
    kill_self(&name);
    let (b, pb) = tt.get().get();
    println("B listening");
    let _ = Handle::stdout().flush();
    kill_self(&name);
    sleep(300);
    println(&format!("B got: {}", finished(&pb)));
    let _ = b.stop();
    0
}

/// Not a Lean program: as `picked_task_reaches_yield_points`, but `p`
/// reaches effect points (`sched::effect`, as an output would) instead of
/// reference reads (review LF3-01, the effect points' half). Two workers.
fn effect_points_in_a_started_task(_: &[String]) -> u32 {
    let p = Task::spawn(
        || loop {
            lean_runtime::sched::effect();
            std::hint::spin_loop();
        },
        PRIO_DEFAULT,
    );
    let _: bool = has_finished(&p);
    let q = Task::spawn(|| (0..1000u64).sum::<u64>(), PRIO_DEFAULT);
    let _ = has_finished(&q);
    let t = Task::spawn(|| 1001u64, PRIO_DEFAULT);
    eprintln(&format!("t = {}", t.get()));
    drop(t);
    let (fp, fq) = (has_finished(&p), has_finished(&q));
    eprintln(&format!("p finished: {fp}, q finished: {fq}"));
    0
}

// ---------------------------------------------------------------------------
// fixes-14: the final run and the event loop (AR-52), the timers and signals
// (HU-01..06), and the stream hand-offs (HR-01..03)

/// Lean's `toString` of an `Option Int` or an `Option Nat`.
fn opt_text(o: &Option<i64>) -> String {
    match o {
        Some(v) => format!("(some {v})"),
        None => "none".into(),
    }
}

// tests/cases/uvloop/timer_due_in_final_run.lean (review AR-52; the twin
// spins args[1] ms instead of the calibrated computation): the timer comes
// due while the final run runs the task on `main`'s stack, with no loop
// context alive; the final run starts one after the task.
fn timer_due_in_final_run(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let len = Ref::new(ms);
    let tm: UTimer = Timer::new(300, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| println("timer fired"),
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/timer_chain_in_final_run.lean (review AR-52; the twin
// spins args[1] ms): timer A's dependent starts timer B, which comes due
// while the final run runs the task, after A's loop context has ended.
fn timer_chain_in_final_run(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let len = Ref::new(ms);
    let tm: UTimer = Timer::new(10, false);
    let p = tm.next(UvPromise::new);
    let _dep = map_task(
        |_: Option<()>| {
            let tb: UTimer = Timer::new(700, false);
            let pb = tb.next(UvPromise::new);
            let _ = map_task(
                |_: Option<()>| println("B fired"),
                pb.result_opt(),
                PRIO_DEFAULT,
                true,
                true,
            );
            drop(pb);
            println("A fired");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    drop(tm);
    sleep(300);
    let _w = as_task(move || spin_ms(len.get()), PRIO_DEFAULT);
    println("main done");
    0
}

// tests/cases/uvloop/timer_effect_order.lean (review HU-01; the twin spins
// args[1] ms in each phase): a timer due at an effect point goes first as a
// due sleeper does. Phase 3 is `Std.Async.sleep 100` as its Lean code
// reads: a one-shot timer, `result?.map (sync := true)`, then the rest in a
// task (`bindTask`, not `sync`).
fn timer_effect_order(args: &[String]) -> u32 {
    let ms = to_nat(&args[1]);
    let ready: Rc<Promise<()>> = Rc::new(Promise::new());
    let r2 = ready.clone();
    let s = as_task(
        move || {
            r2.resolve(());
            drop(r2);
            sleep(200);
            println("phase 1: sleeper woke");
        },
        PRIO_DEFAULT,
    );
    let _ = ready.result_opt().get();
    spin_ms(ms);
    println("phase 1: main computed false");
    s.get();
    let t: UTimer = Timer::new(100, false);
    let p = t.next(UvPromise::new);
    let d = map_task(
        |_: Option<()>| println("phase 2: timer fired"),
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p);
    spin_ms(ms);
    println("phase 2: main computed false");
    d.get();
    let t3: UTimer = Timer::new(100, false);
    let p3 = t3.next(UvPromise::new);
    let m = map_task(
        |o: Option<()>| o.is_some(),
        p3.result_opt(),
        PRIO_DEFAULT,
        true,
        false,
    );
    drop(p3);
    let atk = map_task(
        |ok: bool| {
            println("phase 3: async sleep done");
            ok
        },
        m,
        PRIO_DEFAULT,
        false,
        true,
    );
    spin_ms(ms);
    println("phase 3: main computed false");
    println(&format!("phase 3: async task ok: {}", atk.get()));
    0
}

// tests/cases/uvloop/timer_repeat_zero_held.lean (review HU-02): the loop
// holds a running repeating timer with timeout 0 until `stop`.
fn timer_repeat_zero_held(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]);
    let task = {
        let t: UTimer = Timer::new(ms, true);
        let p0 = t.next(UvPromise::new);
        let _ = p0.result_opt().get();
        drop(p0);
        let p1 = t.next(UvPromise::new);
        p1.result_opt()
    };
    sleep(200);
    println(&format!(
        "dropped: second promise finished: {}",
        has_finished(&task)
    ));
    let u: UTimer = Timer::new(ms, true);
    let q0 = u.next(UvPromise::new);
    let _ = q0.result_opt().get();
    drop(q0);
    let q1 = u.next(UvPromise::new);
    let qt = q1.result_opt();
    drop(q1);
    sleep(200);
    println(&format!(
        "kept: second promise finished before stop: {}",
        has_finished(&qt)
    ));
    u.stop();
    let f = has_finished(&qt);
    println(&format!(
        "after stop: finished {f}, value {}",
        repr_unit(&qt.get())
    ));
    0
}

// tests/cases/uvloop/timer_fresh_next_twice.lean (review HU-03): the
// catch-up of the second extern does not run the tick that came due
// microseconds before.
fn timer_fresh_next_twice(args: &[String]) -> u32 {
    let period = to_nat(&args[0]);
    let t: UTimer = Timer::new(period, true);
    let a = t.next(UvPromise::new);
    let b = t.next(UvPromise::new);
    sleep(200);
    println(&format!(
        "next twice: first finished {}, second finished {}",
        finished(&a),
        finished(&b)
    ));
    drop((a, b));
    t.stop();
    let u: UTimer = Timer::new(period, true);
    let c = u.next(UvPromise::new);
    u.reset();
    sleep(200);
    println(&format!("next then reset: first finished {}", finished(&c)));
    drop(c);
    u.stop();
    0
}

// tests/cases/uvloop/signal_before_timer_in_look.lean (review HU-04; the
// twin spins args[2] ms in timer A's dependent): within one look, the
// signal comes before the timer due.
fn signal_before_timer_in_look(args: &[String]) -> u32 {
    let num: i64 = args[0].parse().expect("an Int");
    let ms = to_nat(&args[2]);
    let len = Ref::new(ms);
    let w: USignal = Signal::new(num as i32, false);
    let pw = uv_ok(w.next(UvPromise::new));
    let _s = map_task(
        |_: Option<i64>| println("signal"),
        pw.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(pw);
    let ta: UTimer = Timer::new(10, false);
    let pa = ta.next(UvPromise::new);
    let _a = map_task(
        move |_: Option<()>| {
            spin_ms(len.get());
            println("A done false");
        },
        pa.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(pa);
    let tb: UTimer = Timer::new(100, false);
    let pb = tb.next(UvPromise::new);
    let _b = map_task(
        |_: Option<()>| println("timer B"),
        pb.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(pb);
    let pid = lio::get_pid();
    // a process spawn is an effect point
    lean_runtime::sched::effect();
    let _child = ok(lio::spawn(
        "sh",
        &["-c", &format!("sleep 0.3; kill -USR1 {pid}")],
        lio::INHERIT,
    ));
    sleep(2000);
    println("main done");
    0
}

// tests/cases/uvloop/signal_batch_new_watcher.lean (review HU-05; the twin
// spins args[3] ms in timer A's dependent): a watcher started by a `sync`
// dependent of an earlier delivery of the batch does not get a later
// signal of the batch.
fn signal_batch_new_watcher(args: &[String]) -> u32 {
    let usr1: i64 = args[0].parse().expect("an Int");
    let usr2: i64 = args[1].parse().expect("an Int");
    let ms = to_nat(&args[3]);
    let len = Ref::new(ms);
    let w0: USignal = Signal::new(usr2 as i32, true);
    let p0 = uv_ok(w0.next(UvPromise::new));
    let _d0 = map_task(
        |v: Option<i64>| println(&format!("W0 got {}", opt_text(&v))),
        p0.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p0);
    let w2task: Ref<Option<Task<Option<i64>>>> = Ref::new(None);
    let w2t = w2task.clone();
    let w1: USignal = Signal::new(usr1 as i32, false);
    let p1 = uv_ok(w1.next(UvPromise::new));
    let _d1 = map_task(
        move |v: Option<i64>| {
            let w2: USignal = Signal::new(usr2 as i32, false);
            let p2 = uv_ok(w2.next(UvPromise::new));
            let _ = map_task(
                |v: Option<i64>| println(&format!("W2 got {}", opt_text(&v))),
                p2.result_opt(),
                PRIO_DEFAULT,
                true,
                true,
            );
            w2t.set(Some(p2.result_opt()));
            drop(p2);
            println(&format!("W1 got {}, started W2", opt_text(&v)));
        },
        p1.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p1);
    let ta: UTimer = Timer::new(10, false);
    let pa = ta.next(UvPromise::new);
    let _a = map_task(
        move |_: Option<()>| {
            spin_ms(len.get());
            println("A done false");
        },
        pa.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(pa);
    let pid = lio::get_pid();
    lean_runtime::sched::effect();
    let _child = ok(lio::spawn(
        "sh",
        &[
            "-c",
            &format!("sleep 0.3; kill -USR1 {pid}; sleep 0.1; kill -USR2 {pid}"),
        ],
        lio::INHERIT,
    ));
    sleep(2000);
    match w2task.get() {
        Some(t) => println(&format!("W2 finished: {}", has_finished(&t))),
        None => println("W2 not started"),
    }
    0
}

// tests/cases/uvloop/timer_period_from_look.lean (review HU-06; the twin
// spins args[1] ms in `main` and args[2] ms in X): tick 2's period starts
// from the look that found tick 1 due, not from its callback, which runs
// after X.
fn timer_period_from_look(args: &[String]) -> u32 {
    let (main_ms, x_ms) = (to_nat(&args[1]), to_nat(&args[2]));
    let len_x = Ref::new(x_ms);
    let t: UTimer = Timer::new(1000, true);
    let p0 = t.next(UvPromise::new);
    let _ = p0.result_opt().get();
    drop(p0);
    let t0 = mono_ms_now();
    let p1 = t.next(UvPromise::new);
    let tick2: Ref<Option<Task<Option<()>>>> = Ref::new(None);
    let (tm, k2) = (t.clone(), tick2.clone());
    let _d1 = map_task(
        move |_: Option<()>| {
            let p2 = tm.next(UvPromise::new);
            k2.set(Some(p2.result_opt()));
            drop(p2);
        },
        p1.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    drop(p1);
    let ready: Rc<Promise<()>> = Rc::new(Promise::new());
    let r2 = ready.clone();
    let xt = as_task(
        move || {
            let n = len_x.get();
            r2.resolve(());
            drop(r2);
            sleep(300);
            spin_ms(n);
            false
        },
        PRIO_DEFAULT,
    );
    let _ = ready.result_opt().get();
    spin_ms(main_ms);
    let now = mono_ms_now();
    sleep((t0 + 3200).saturating_sub(now) as u32);
    let f = match tick2.get() {
        Some(t2) => has_finished(&t2),
        None => false,
    };
    println(&format!("tick 2 by then: {f}"));
    println(&format!("X: ok: {}", xt.get()));
    0
}

/// A handle's last reference dropped by the translator's free (a drain, in
/// the no-suspend scope), then the glue's drain-end hook
/// (`sched::after_drain`): where natively the drop's `fclose` returned.
fn drop_in_drain<T>(v: T) {
    {
        let _scope = lean_runtime::sched::no_suspend();
        drop(v);
    }
    lean_runtime::sched::after_drain();
}

/// `IO.Process.spawn { cmd := "sh", args := #["-c", script], stdin := .piped }`
/// then `takeStdin`, `write` of 65536 bytes (as much as an empty pipe
/// takes), `flush` and `putStr "x"`: the handle's last byte waits in its
/// buffer, which the pipe cannot take until the child reads.
fn child_with_full_stdin(script: &str) -> (Handle, lean_runtime::io::process::ChildProcess) {
    lean_runtime::sched::effect();
    let child = ok(lio::spawn(
        "sh",
        &["-c", script],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    ));
    let stdin = child.stdin.expect("piped");
    ok(stdin.write(&[b'x'; 65536]));
    ok(stdin.flush());
    ok(stdin.put_str(b"x"));
    (stdin, child.process)
}

// tests/cases/process/handoff_then_resolve_again.lean (review HR-01; the
// glue tests the promise inside `sched::resolve`'s store, `Promise::resolve`)
fn handoff_then_resolve_again(_: &[String]) -> u32 {
    let p: Rc<Promise<i64>> = Rc::new(Promise::new());
    let pb = p.clone();
    let b = as_task(
        move || {
            sleep(300);
            pb.resolve(2);
            let v = pb.result_opt().get();
            println(&format!("B sees {}", opt_text(&v)));
        },
        PRIO_DEFAULT,
    );
    let (stdin, child) = child_with_full_stdin("sleep 1; cat > /dev/null");
    drop_in_drain(stdin);
    p.resolve(1);
    let v = p.result_opt().get();
    println(&format!("main sees {}", opt_text(&v)));
    b.get();
    let _ = child.wait();
    0
}

// tests/cases/process/handoff_then_sync_map.lean (review HR-02): the drain's
// end waits for the handed-off writer, so `t` has finished when the glue
// asks `dependent_runs_now`, and the function runs at once on `main`.
fn handoff_then_sync_map(_: &[String]) -> u32 {
    let t = as_task(
        || {
            sleep(300);
            5u64
        },
        PRIO_DEFAULT,
    );
    let slow = as_task(
        || {
            sleep(1500);
            7u64
        },
        PRIO_DEFAULT,
    );
    let (stdin, child) = child_with_full_stdin("sleep 1; cat > /dev/null");
    drop_in_drain(stdin);
    let d = map_task(
        move |v: u64| {
            let w = slow.get();
            println(&format!("dep {v} {w}"));
        },
        t,
        PRIO_DEFAULT,
        true,
        true,
    );
    println("main after mapTask");
    d.get();
    let _ = child.wait();
    0
}

// tests/cases/process/handoff_in_tree_then_sync_map.lean (leanrs's deep
// drain of HR-02): the handle is dropped with the array that holds it, in
// its drain (`Arr`, a `DrainScope`), whose end waits for the writer.
fn handoff_in_tree_then_sync_map(_: &[String]) -> u32 {
    let t = as_task(
        || {
            sleep(300);
            5u64
        },
        PRIO_DEFAULT,
    );
    let slow = as_task(
        || {
            sleep(1500);
            7u64
        },
        PRIO_DEFAULT,
    );
    let (stdin, child) = child_with_full_stdin("sleep 1; cat > /dev/null");
    // `Tree.node none #[Tree.node (some stdin) #[]]`, dropped after
    // `Tree.count`'s last use
    let tree = Arr::new(vec![Arr::new(vec![Some(stdin)])]);
    drop(tree);
    let d = map_task(
        move |v: u64| {
            let w = slow.get();
            println(&format!("dep {v} {w}"));
        },
        t,
        PRIO_DEFAULT,
        true,
        true,
    );
    println("main after mapTask");
    d.get();
    let _ = child.wait();
    0
}

// tests/cases/process/deferred_resolve_before_handoff.lean (review RF14-07):
// one free (an `Arr`'s drain, which drops the last element first, as
// `lean_del_core`) drops the promise, then the child's stdin, whose last
// byte goes to a writer thread; the promise's deferred resolution waits only
// for the writers handed off before it, so the reader reads the child's
// stdout, and the child then reads its stdin.
fn deferred_resolve_before_handoff(_: &[String]) -> u32 {
    lean_runtime::sched::effect();
    let child = ok(lio::spawn(
        "sh",
        &["-c", "head -c 70000 /dev/zero; cat > /dev/null"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    ));
    let stdin = child.stdin.expect("piped");
    let stdout = child.stdout.expect("piped");
    let p: Promise<()> = Promise::new();
    let r = p.result_opt();
    let reader = as_task(
        move || {
            let _ = r.get();
            ok(lio::read_to_end(&stdout)).len() as u64
        },
        PRIO_DEFAULT,
    );
    // the reader runs and blocks on the promise
    sleep(50);
    ok(stdin.write(&[b'x'; 65536]));
    ok(stdin.flush());
    ok(stdin.put_str(b"x"));
    /// `Sum IO.FS.Handle (IO.Promise Unit)`.
    #[allow(dead_code)]
    enum E {
        H(Handle),
        P(Promise<()>),
    }
    let arr = Arr::new(vec![E::H(stdin), E::P(p)]);
    drop(arr);
    println(&format!("reader got {}", reader.get()));
    let st = ok(child.process.wait());
    println(&format!("child {st}"));
    0
}

/// Not a Lean program: review RF14-03's gap. With one worker
/// (`LEAN_NUM_THREADS=1`), a pool task runs on `main`'s stack and calls
/// `depend` on a finished source (as when `depend`'s writers point let it
/// finish); the dependent runs at once, as Lean's fast path, in the pool
/// task's frame, and waits for a queued task. Natively `wait_for` raises
/// the worker limit for the pool task, and the queued task runs; before
/// the fix the dependent's entry hid the pool task, so the wait kept the
/// worker, and the program hung.
fn rf14_fast_pool_caller_waits(_: &[String]) -> u32 {
    use lean_runtime::sched::{self, Outcome, TaskId};
    let p = as_task(
        || {
            let ran = Rc::new(std::cell::Cell::new(false));
            let r2 = ran.clone();
            sched::depend(
                TaskId::FINISHED,
                Box::new(move || {
                    let q = Task::spawn(|| 7u64, PRIO_DEFAULT);
                    r2.set(q.get() == 7);
                    Outcome::Done
                }),
                PRIO_DEFAULT,
                true,
                true,
            );
            ran.get()
        },
        PRIO_DEFAULT,
    );
    println(&format!(
        "the function's wait ran the queued task: {}",
        p.get()
    ));
    0
}

/// Not a Lean program: review RF14-03. `process/handoff_then_sync_map`
/// without the drain-end hook (a glue that has not adopted it, or a drain
/// nested in an outer no-suspend scope): `dependent_runs_now` sees `t`
/// unfinished, and `depend`'s writers point lets it finish, so `depend`
/// runs the `sync` dependent at once. It runs as Lean's fast path (the
/// function applied in the caller), so its waits, for the finished `t` and
/// for the unfinished `slow`, print no "`Task.get` called from a `(sync :=
/// true)` task"; before RF14-03 it ran as a `sync` task, and the wait for
/// `slow` printed it.
fn rf14_depend_fast_path(_: &[String]) -> u32 {
    let t = as_task(
        || {
            sleep(300);
            5u64
        },
        PRIO_DEFAULT,
    );
    let slow = as_task(
        || {
            sleep(1500);
            7u64
        },
        PRIO_DEFAULT,
    );
    let (stdin, child) = child_with_full_stdin("sleep 1; cat > /dev/null");
    {
        // the translator's free path, without the drain-end hook
        let _scope = lean_runtime::sched::no_suspend();
        drop(stdin);
    }
    let t2 = t.clone();
    let d = map_task(
        move |v: u64| {
            let u = t2.get();
            let w = slow.get();
            println(&format!("dep {v} {u} {w}"));
        },
        t,
        PRIO_DEFAULT,
        true,
        true,
    );
    println("main after mapTask");
    d.get();
    let _ = child.wait();
    0
}

// tests/cases/process/handoff_then_try_lock.lean (review HR-03): each try
// function is the context's first writers point after the hand-off; the
// port leaves the drain-end hook out (a glue that calls it joins the writer
// there, and the try then finds none to wait for).
fn handoff_then_try_lock(_: &[String]) -> u32 {
    fn round(name: &str, take: impl FnOnce() + 'static, attempt: impl FnOnce() -> bool) {
        let started: Rc<Promise<()>> = Rc::new(Promise::new());
        let s2 = started.clone();
        let taker = as_task(
            move || {
                s2.resolve(());
                drop(s2);
                sleep(150);
                take();
            },
            PRIO_DEDICATED,
        );
        let _ = started.result_opt().get();
        let (stdin, child) = child_with_full_stdin("sleep 0.5; cat > /dev/null");
        {
            // the translator's free path, without the drain-end hook
            let _scope = lean_runtime::sched::no_suspend();
            drop(stdin);
        }
        let got = attempt();
        println(&format!("{name}: {got}"));
        taker.get();
        let _ = child.wait();
    }
    use lean_runtime::sched::sync::{Mutex, RecursiveMutex, SharedMutex};
    let m = Rc::new(Mutex::new());
    let m2 = m.clone();
    round("BaseMutex.tryLock", move || m2.lock(), || m.try_lock());
    let r = Rc::new(RecursiveMutex::new());
    let r2 = r.clone();
    round(
        "BaseRecursiveMutex.tryLock",
        move || r2.lock(),
        || r.try_lock(),
    );
    let s = Rc::new(SharedMutex::new());
    let s2 = s.clone();
    round(
        "BaseSharedMutex.tryWrite",
        move || s2.write(),
        || s.try_write(),
    );
    let w = Rc::new(SharedMutex::new());
    let w2 = w.clone();
    round(
        "BaseSharedMutex.tryRead",
        move || w2.write(),
        || w.try_read(),
    );
    0
}

// tests/cases/uvloop/loop_deep_sync_dependent.lean (fixes-16, hunt HSK-03):
// def main (args : List String) : IO Unit := do
//   let d := args[0]!.toNat!
//   let tm ← Timer.mk 100 false
//   let p ← tm.next
//   let r := p.result!.map (sync := true) fun _ => deep d
//   IO.println s!"loop {r.get}"
fn loop_deep_sync_dependent(args: &[String]) -> u32 {
    let d = to_nat(&args[0]);
    let tm: UTimer = Timer::new(100, false);
    let p = tm.next(UvPromise::new);
    let r = map_task(
        move |_: ()| crate::cases::deep_levels(d),
        p.result_bang(),
        PRIO_DEFAULT,
        true,
        false,
    );
    println(&format!("loop {}", r.get()));
    drop((p, tm));
    0
}
