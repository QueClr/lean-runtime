//! Rust-level ports of `tests/cases/{tasks,sync,refs,taskio}/*.lean`, line
//! by line, as a translator's output would read: the same tasks, values and
//! effects in the same order, and each value dropped where compiled Lean
//! releases it.
//!
//! Both drivers compile this file: `sched-cases` (this package) over the
//! single-thread scheduler, and `sched-cases-mt` (`tests/sched-driver-mt`,
//! by a `#[path]` include) over threads mode, `sched::mt`. So a port names
//! only what both drivers' `glue`, `lean` and `lio` modules define: a
//! counted object shared with a task is an `Obj` (`Rc`, or `Arc` in threads
//! mode), a cell a task writes is a `Var` (`RefCell`, or a `Mutex`), and the
//! closures a task runs capture only such values. The single-thread
//! driver's other ports (uvloop, the io and process cases with tasks, its
//! own programs) are in `cases_st.rs`; a case here whose port depends on
//! what only the single-thread scheduler has goes in `SINGLE_THREAD_ONLY`.

use crate::glue::{eprintln, println};
use crate::lean::*;
use crate::lio::{self, quote, R};
use lean_runtime::io::process::{Stdio, StdioConfig};
use lean_runtime::io::{FsMode, Handle};
use lean_runtime::sched::sync::{Condvar, Mutex, RecursiveMutex, SharedMutex};

/// A case: its initializer (Lean's `initialize` declarations) and `main`.
pub type Case = (fn(), fn(&[String]) -> u32);

/// No initializer (Lean's module has no `initialize` declaration).
fn no_init() {}

/// The cases both drivers run: the id, its initializer and `main`.
pub const CASES: &[(&str, Case)] = &[
    (
        "checkcanceled_after_main",
        (no_init, checkcanceled_after_main),
    ),
    ("dropped_pure_task", (no_init, dropped_pure_task)),
    (
        "exit_joins_before_flush",
        (no_init, exit_joins_before_flush),
    ),
    ("hasfinished_spin", (no_init, hasfinished_spin)),
    (
        "runaway_io_task_unawaited",
        (no_init, runaway_io_task_unawaited),
    ),
    (
        "runaway_pure_task_referenced",
        (init_keep, runaway_pure_task_referenced),
    ),
    (
        "runaway_pure_task_started",
        (no_init, runaway_pure_task_started),
    ),
    ("sleep_polling_spin", (no_init, sleep_polling_spin)),
    ("sync_dependent_order", (no_init, sync_dependent_order)),
    ("promise_across_tasks", (no_init, promise_across_tasks)),
    ("stack_overflow_in_task", (no_init, stack_overflow_in_task)),
    ("mutex_handoff", (no_init, mutex_handoff)),
    ("condvar_turns", (no_init, condvar_turns)),
    ("shared_mutex_readers", (no_init, shared_mutex_readers)),
    ("recursive_mutex", (no_init, recursive_mutex)),
    ("pure_chain_io_dep", (no_init, pure_chain_io_dep)),
    ("pure_bind_io_dep", (no_init, pure_bind_io_dep)),
    ("exit_from_task", (no_init, exit_from_task)),
    ("get_in_sync_task", (no_init, get_in_sync_task)),
    ("promise_result_opt", (no_init, promise_result_opt)),
    ("result_bang_some", (no_init, result_bang_some)),
    ("result_bang_dropped", (no_init, result_bang_dropped)),
    (
        "result_bang_dropped_in_task",
        (no_init, result_bang_dropped_in_task),
    ),
    (
        "result_bang_dropped_abort",
        (no_init, result_bang_dropped_abort),
    ),
    (
        "result_bang_dropped_first",
        (no_init, result_bang_dropped_first),
    ),
    ("task_pure_graph", (no_init, task_pure_graph)),
    (
        "result_bang_dropped_redirected",
        (no_init, result_bang_dropped_redirected),
    ),
    (
        "get_in_sync_task_redirected",
        (no_init, get_in_sync_task_redirected),
    ),
    (
        "cancel_promise_and_pure",
        (no_init, cancel_promise_and_pure),
    ),
    ("result_bang_dep_order", (no_init, result_bang_dep_order)),
    ("pure_get_in_sync_task", (no_init, pure_get_in_sync_task)),
    (
        "sync_dependent_before_waiter",
        (no_init, sync_dependent_before_waiter),
    ),
    (
        "dropped_promise_waiter_wakes",
        (no_init, dropped_promise_waiter_wakes),
    ),
    (
        "dropped_promise_waiter_unrelated_finish",
        (no_init, dropped_promise_waiter_unrelated_finish),
    ),
    (
        "waiter_wakes_after_nested_finish",
        (no_init, waiter_wakes_after_nested_finish),
    ),
    (
        "sync_walk_stuck_unrelated_finish",
        (no_init, sync_walk_stuck_unrelated_finish),
    ),
    (
        "wait_any_wakes_on_finish",
        (no_init, wait_any_wakes_on_finish),
    ),
    (
        "sync_walk_mutex_unrelated_finish",
        (no_init, sync_walk_mutex_unrelated_finish),
    ),
    ("sync_walk_mutex_alone", (no_init, sync_walk_mutex_alone)),
    ("sync_walk_stuck_alone", (no_init, sync_walk_stuck_alone)),
    (
        "sync_walk_mutex_unref_finish",
        (no_init, sync_walk_mutex_unref_finish),
    ),
    ("wait_any_unref_finish", (no_init, wait_any_unref_finish)),
    ("wait_any_pure_stalled", (no_init, wait_any_pure_stalled)),
    ("wait_queue_order", (no_init, wait_queue_order)),
    (
        "wait_head_blocks_on_main",
        (no_init, wait_head_blocks_on_main),
    ),
    (
        "wait_any_head_blocks_on_main",
        (no_init, wait_any_head_blocks_on_main),
    ),
    ("wait_any_keeps_worker", (no_init, wait_any_keeps_worker)),
    ("poll_queue_order", (no_init, poll_queue_order)),
    ("poll_threshold_promise", (no_init, poll_threshold_promise)),
    ("poll_threshold_mutex", (no_init, poll_threshold_mutex)),
    ("wait_picked_pure", (no_init, wait_picked_pure)),
    ("wait_any_picked_pure", (no_init, wait_any_picked_pure)),
    (
        "wait_any_finished_unnotified",
        (no_init, wait_any_finished_unnotified),
    ),
    ("self_wait_frees_worker", (no_init, self_wait_frees_worker)),
    (
        "runaway_pure_task_before_io",
        (no_init, runaway_pure_task_before_io),
    ),
    ("sync_walk_keeps_worker", (no_init, sync_walk_keeps_worker)),
    (
        "sync_self_wait_keeps_worker",
        (no_init, sync_self_wait_keeps_worker),
    ),
    (
        "sync_wait_in_inline_walk",
        (no_init, sync_wait_in_inline_walk),
    ),
    (
        "sync_dep_waits_queued_task",
        (no_init, sync_dep_waits_queued_task),
    ),
    ("wait_pure_queue_order", (no_init, wait_pure_queue_order)),
    (
        "drop_queued_behind_pure",
        (no_init, drop_queued_behind_pure),
    ),
    (
        "runaway_pure_before_awaited",
        (no_init, runaway_pure_before_awaited),
    ),
    (
        "picked_task_own_worker_streams",
        (no_init, picked_task_own_worker_streams),
    ),
    (
        "picked_task_sleeping_worker",
        (no_init, picked_task_sleeping_worker),
    ),
    (
        "picked_task_ticking_worker",
        (no_init, picked_task_ticking_worker),
    ),
    (
        "picked_task_reaches_yield_points",
        (no_init, picked_task_reaches_yield_points),
    ),
    (
        "runaway_pure_passed_over",
        (no_init, runaway_pure_passed_over),
    ),
    (
        "worker_streams_closed_at_exit",
        (no_init, worker_streams_closed_at_exit),
    ),
    (
        "worker_streams_at_process_exit",
        (no_init, worker_streams_at_process_exit),
    ),
    // review AR-34 (fixes-4)
    (
        "worker_streams_before_dedicated",
        (no_init, worker_streams_before_dedicated),
    ),
    (
        "picked_task_short_sleeper_long",
        (no_init, picked_task_short_sleeper_long),
    ),
    ("picked_task_watchdog", (no_init, picked_task_watchdog)),
    ("picked_task_sleep_zero", (no_init, picked_task_sleep_zero)),
    ("late_task_after_main", (no_init, late_task_after_main)),
    (
        "late_dependent_of_dedicated",
        (no_init, late_dependent_of_dedicated),
    ),
    ("late_wait_dedicated", (no_init, late_wait_dedicated)),
    ("late_wait_pool", (no_init, late_wait_pool)),
    ("late_pool_child_runs", (no_init, late_pool_child_runs)),
    (
        "late_dedicated_child_runs",
        (no_init, late_dedicated_child_runs),
    ),
    (
        "main_waits_dedicated_child",
        (no_init, main_waits_dedicated_child),
    ),
    ("wait_dep_mid_walk", (no_init, wait_dep_mid_walk)),
    (
        "wait_dep_mid_promise_walk",
        (no_init, wait_dep_mid_promise_walk),
    ),
    ("poll_dep_mid_walk", (no_init, poll_dep_mid_walk)),
    ("wait_any_own_dep", (no_init, wait_any_own_dep)),
    ("task_waits_own_dep", (no_init, task_waits_own_dep)),
    ("sync_dep_waits_older", (no_init, sync_dep_waits_older)),
    ("lost_update", (no_init, lost_update)),
    ("set_during_modify", (no_init, set_during_modify)),
    ("get_during_modify", (no_init, get_during_modify)),
    ("swap_during_modify", (no_init, swap_during_modify)),
    ("worker_keeps_streams", (no_init, worker_keeps_streams)),
    ("worker_keeps_errno", (no_init, worker_keeps_errno)),
    // tests/cases/taskio: blocking IO in programs with tasks (sched-io)
    ("output_big_stdout", (no_init, output_big_stdout)),
    ("output_both_overflow", (no_init, output_both_overflow)),
    ("task_reads_main_writes", (no_init, task_reads_main_writes)),
    ("wait_in_task", (no_init, wait_in_task)),
    ("output_while_ticking", (no_init, output_while_ticking)),
    // threads mode, batch T3: cases that need real contention
    ("wait_chain_beyond_pool", (no_init, wait_chain_beyond_pool)),
    ("wait_any_faster", (no_init, wait_any_faster)),
    (
        "stack_overflow_in_dedicated",
        (no_init, stack_overflow_in_dedicated),
    ),
    (
        "late_tasks_while_enqueuing",
        (no_init, late_tasks_while_enqueuing),
    ),
];

/// The cases of `CASES` that threads mode (`tests/sched-driver-mt`) does
/// not run, each with the reason: their ports depend on what only the
/// single-thread scheduler has (its yield points, a known difference
/// LSCHED-xx of the deferred model). Threads mode runs every other case of
/// `CASES`. An outcome in threads mode that native never gave is not a
/// reason to list a case here: it is a bug, or a native schedule to record
/// from native runs (docs/threads.md, section 4).
// read by `sched-cases-mt` only
#[allow(dead_code)]
pub const SINGLE_THREAD_ONLY: &[(&str, &str)] = &[];

/// The case `id` of `CASES`.
pub fn lookup(id: &str) -> Option<Case> {
    CASES.iter().find(|(n, _)| *n == id).map(|&(_, c)| c)
}

/// `partial def spin (x acc : UInt64) : UInt64` (a tail call: a loop).
fn spin(mut x: u64, mut acc: u64) -> u64 {
    while x != 0 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        acc = acc.wrapping_add(1);
    }
    acc
}

/// `args.head!.toNat!.toUInt64 ||| 1`.
fn seed(args: &[String]) -> u64 {
    to_nat(&args[0]) | 1
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let _ ← IO.asTask do
//     IO.sleep ms.toUInt32
//     let c ← IO.checkCanceled
//     IO.eprintln s!"canceled after main: {c}"
//   IO.println "main returns"
fn checkcanceled_after_main(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            let c = check_canceled();
            eprintln(&format!("canceled after main: {c}"));
        },
        PRIO_DEFAULT,
    );
    println("main returns");
    0
}

// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let t := Task.spawn fun _ => spin s 0
//   let f ← IO.hasFinished t
//   IO.eprintln s!"main done {f}"
fn dropped_pure_task(args: &[String]) -> u32 {
    let s = seed(args);
    let t = Task::spawn(move || spin(s, 0), PRIO_DEFAULT);
    let f = has_finished(&t);
    // `IO.hasFinished` borrows `t`; its last use, so released here.
    drop(t);
    eprintln(&format!("main done {f}"));
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let _ ← IO.asTask do
//     IO.sleep ms.toUInt32
//     IO.eprintln "task done"
//   IO.println "main done"
fn exit_joins_before_flush(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            eprintln("task done");
        },
        PRIO_DEFAULT,
    );
    println("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let t ← IO.asTask do
//     let mut s := 0
//     for i in [0:n] do
//       s := s + i
//     return s
//   while !(← IO.hasFinished t) do
//     pure ()
//   match t.get with
//   | .ok s => IO.println s!"finished: {s}"
//   | .error e => IO.println s!"error: {e}"
fn hasfinished_spin(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let t = as_task(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    while !has_finished(&t) {}
    let s = t.get();
    println(&format!("finished: {s}"));
    0
}

// partial def spinIO (r : IO.Ref UInt64) : IO Unit := do
//   r.modify (· + 1)
//   spinIO r
//
// def main (args : List String) : IO Unit := do
//   let r ← IO.mkRef args.length.toUInt64
//   let _t ← IO.asTask (spinIO r)
//   IO.println "main done"
fn runaway_io_task_unawaited(args: &[String]) -> u32 {
    let r = Ref::new(args.len() as u64);
    let _t = as_task(
        move || loop {
            r.modify(|v| v.wrapping_add(1));
        },
        PRIO_DEFAULT,
    );
    drop(_t);
    println("main done");
    0
}

// initialize keep : IO.Ref (Option (Task UInt64)) ← IO.mkRef none
//
// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let t := Task.spawn fun _ => spin s 0
//   keep.set (some t)
//   IO.eprintln "main done"
// `keep` is read on `main`'s thread only, so a thread-local stands for the
// initialized constant in both drivers.
thread_local! {
    static KEEP: std::cell::RefCell<Option<Ref<Option<Task<u64>>>>> = const { std::cell::RefCell::new(None) };
}

fn init_keep() {
    KEEP.with(|k| *k.borrow_mut() = Some(Ref::new(None)));
}

fn runaway_pure_task_referenced(args: &[String]) -> u32 {
    let s = seed(args);
    let t = Task::spawn(move || spin(s, 0), PRIO_DEFAULT);
    let keep = KEEP.with(|k| k.borrow().clone().unwrap());
    keep.set(Some(t));
    eprintln("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let ms := args[1]!.toNat!
//   let t := Task.spawn fun _ => spin s 0
//   let f1 ← IO.hasFinished t
//   IO.sleep ms.toUInt32
//   let f2 ← IO.hasFinished t
//   IO.eprintln s!"main done {f1} {f2}"
fn runaway_pure_task_started(args: &[String]) -> u32 {
    let s = seed(args);
    let ms = to_nat(&args[1]) as u32;
    let t = Task::spawn(move || spin(s, 0), PRIO_DEFAULT);
    let f1 = has_finished(&t);
    sleep(ms);
    let f2 = has_finished(&t);
    drop(t);
    eprintln(&format!("main done {f1} {f2}"));
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let flag ← IO.mkRef false
//   let _ ← IO.asTask do
//     IO.sleep ms.toUInt32
//     flag.set true
//   while !(← flag.get) do
//     IO.sleep 1
//   IO.println "flag seen"
fn sleep_polling_spin(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let flag = Ref::new(false);
    let f2 = flag.clone();
    let _ = as_task(
        move || {
            sleep(ms);
            f2.set(true);
        },
        PRIO_DEFAULT,
    );
    while !flag.get() {
        sleep(1);
    }
    println("flag seen");
    0
}

// def main (args : List String) : IO Unit := do
//   let blockMs := args[0]!.toNat!
//   let gapMs := args[1]!.toNat!
//   let b ← IO.asTask (do IO.sleep blockMs.toUInt32; IO.println "blocker")
//   IO.sleep gapMs.toUInt32
//   let _ ← IO.mapTask (fun _ => IO.println "async dep") b
//   let _ ← IO.mapTask (sync := true) (fun _ => do
//     let _ ← IO.asTask (IO.println "made by sync dep")) b
//   let _ ← IO.mapTask (fun _ => IO.println "newest async dep") b
fn sync_dependent_order(args: &[String]) -> u32 {
    let block_ms = to_nat(&args[0]) as u32;
    let gap_ms = to_nat(&args[1]) as u32;
    let b = as_task(
        move || {
            sleep(block_ms);
            println("blocker");
        },
        PRIO_DEFAULT,
    );
    sleep(gap_ms);
    let _ = map_task(
        |()| println("async dep"),
        b.clone(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let _ = map_task(
        |()| {
            let _ = as_task(|| println("made by sync dep"), PRIO_DEFAULT);
        },
        b.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let _ = map_task(
        |()| println("newest async dep"),
        b,
        PRIO_DEFAULT,
        false,
        true,
    );
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let p ← IO.Promise.new (α := Nat)
//   let t ← IO.asTask (prio := .dedicated) do
//     let v ← IO.wait p.result!
//     return v + 1
//   IO.sleep ms.toUInt32
//   IO.println s!"task finished before resolve: {← IO.hasFinished t}"
//   p.resolve 41
//   match ← IO.wait t with
//   | .ok v => IO.println s!"task got {v}"
//   | .error e => IO.println s!"error {e}"
//   let q ← IO.Promise.new (α := Nat)
//   let r := q.result?
//   IO.println s!"dropped promise: {(← IO.wait r).isNone}"
fn promise_across_tasks(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let p: Promise<u64> = Promise::new();
    let result = p.result_bang();
    let t = as_task(move || result.get() + 1, PRIO_DEDICATED);
    sleep(ms);
    println(&format!(
        "task finished before resolve: {}",
        has_finished(&t)
    ));
    p.resolve(41);
    println(&format!("task got {}", t.get()));
    let q: Promise<u64> = Promise::new();
    let r = q.result_opt();
    // `result?` borrows the promise: its last use, so released here.
    drop(q);
    println(&format!("dropped promise: {}", r.get().is_none()));
    0
}

// def deep : Nat → Nat
//   | 0 => 0
//   | n + 1 => deep n + 1
//
// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   IO.println "main starts"
//   let _ ← IO.asTask (do IO.println s!"depth {deep n}")
//   IO.sleep 1000
//   IO.eprintln "not reached"
pub(crate) fn deep(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        std::hint::black_box(deep(std::hint::black_box(n - 1))) + 1
    }
}

fn stack_overflow_in_task(args: &[String]) -> u32 {
    stack_overflow_at(args, PRIO_DEFAULT)
}

// The same with `IO.asTask (prio := .dedicated)`
// (`tests/cases/tasks/stack_overflow_in_dedicated.lean`).
fn stack_overflow_in_dedicated(args: &[String]) -> u32 {
    stack_overflow_at(args, PRIO_DEDICATED)
}

fn stack_overflow_at(args: &[String], prio: u64) -> u32 {
    let n = to_nat(&args[0]);
    println("main starts");
    let _ = as_task(move || println(&format!("depth {}", deep(n))), prio);
    sleep(1000);
    eprintln("not reached");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let m ← BaseMutex.new
//   let log ← IO.mkRef (#[] : Array String)
//   m.lock
//   let t ← IO.asTask (prio := .dedicated) do
//     log.modify (·.push s!"task tries: {← m.tryLock}")
//     m.lock
//     log.modify (·.push "task has the lock")
//     m.unlock
//   IO.sleep ms.toUInt32
//   log.modify (·.push s!"main tries its own lock: {← m.tryLock}")
//   log.modify (·.push "main unlocks")
//   m.unlock
//   let _ ← IO.wait t
//   for l in ← log.get do IO.println l
fn push(log: &Ref<Vec<String>>, s: String) {
    log.modify(|mut v| {
        v.push(s);
        v
    });
}

fn mutex_handoff(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let m = Obj::new(Mutex::new());
    let log = Ref::new(Vec::new());
    m.lock();
    let (m2, log2) = (m.clone(), log.clone());
    let t = as_task(
        move || {
            let r = m2.try_lock();
            push(&log2, format!("task tries: {r}"));
            m2.lock();
            push(&log2, "task has the lock".into());
            m2.unlock();
        },
        PRIO_DEDICATED,
    );
    sleep(ms);
    let r = m.try_lock();
    push(&log, format!("main tries its own lock: {r}"));
    push(&log, "main unlocks".into());
    m.unlock();
    t.get();
    for l in log.get() {
        println(&l);
    }
    0
}

// def player (m : BaseMutex) (cv : Condvar) (turn : IO.Ref Nat) (me rounds : Nat)
//     (log : IO.Ref (Array String)) : IO Unit := do
//   for i in [0:rounds] do
//     m.lock
//     while (← turn.get) % 2 != me do
//       cv.wait m
//     log.modify (·.push s!"player {me} round {i}")
//     turn.modify (· + 1)
//     m.unlock
//     cv.notifyAll
//
// def main (args : List String) : IO Unit := do
//   let rounds := args.head!.toNat!
//   let m ← BaseMutex.new
//   let cv ← Condvar.new
//   let turn ← IO.mkRef 0
//   let log ← IO.mkRef #[]
//   let a ← IO.asTask (prio := .dedicated) (player m cv turn 1 rounds log)
//   let b ← IO.asTask (prio := .dedicated) (player m cv turn 0 rounds log)
//   let _ ← IO.wait a
//   let _ ← IO.wait b
//   for l in ← log.get do IO.println l
fn player(
    m: Obj<Mutex>,
    cv: Obj<Condvar>,
    turn: Ref<u64>,
    me: u64,
    rounds: u64,
    log: Ref<Vec<String>>,
) {
    for i in 0..rounds {
        m.lock();
        while turn.get() % 2 != me {
            cv.wait(&m);
        }
        push(&log, format!("player {me} round {i}"));
        turn.modify(|t| t + 1);
        m.unlock();
        cv.notify_all();
    }
}

fn condvar_turns(args: &[String]) -> u32 {
    let rounds = to_nat(&args[0]);
    let m = Obj::new(Mutex::new());
    let cv = Obj::new(Condvar::new());
    let turn = Ref::new(0u64);
    let log = Ref::new(Vec::new());
    let (m1, cv1, turn1, log1) = (m.clone(), cv.clone(), turn.clone(), log.clone());
    let a = as_task(
        move || player(m1, cv1, turn1, 1, rounds, log1),
        PRIO_DEDICATED,
    );
    let log2 = log.clone();
    let b = as_task(move || player(m, cv, turn, 0, rounds, log2), PRIO_DEDICATED);
    a.get();
    b.get();
    for l in log.get() {
        println(&l);
    }
    0
}

// def reader (s : BaseSharedMutex) (inside : IO.Promise Unit) (leave : IO.Promise Unit) : IO Unit := do
//   s.read
//   inside.resolve ()
//   let _ ← IO.wait leave.result?
//   s.unlockRead
//
// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let s ← BaseSharedMutex.new
//   let p1 ← IO.Promise.new (α := Unit)
//   let p2 ← IO.Promise.new (α := Unit)
//   let leave ← IO.Promise.new (α := Unit)
//   let r1 ← IO.asTask (prio := .dedicated) (reader s p1 leave)
//   let r2 ← IO.asTask (prio := .dedicated) (reader s p2 leave)
//   let _ ← IO.wait p1.result?
//   let _ ← IO.wait p2.result?
//   let w ← s.tryWrite
//   let r ← s.tryRead
//   IO.println s!"two readers inside: tryWrite {w}, tryRead {r}"
//   s.unlockRead
//   let wr ← IO.asTask (prio := .dedicated) do
//     s.write
//     IO.println "writer entered"
//     s.unlockWrite
//   IO.sleep ms.toUInt32
//   IO.println "readers leave"
//   leave.resolve ()
//   let _ ← IO.wait wr
//   let _ ← IO.wait r1
//   let _ ← IO.wait r2
//   IO.println s!"after: tryWrite {← s.tryWrite}"
fn reader(s: Obj<SharedMutex>, inside: Obj<Promise<()>>, leave: Obj<Promise<()>>) {
    s.read();
    inside.resolve(());
    leave.result_opt().get();
    s.unlock_read();
}

fn shared_mutex_readers(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let s = Obj::new(SharedMutex::new());
    let p1 = Obj::new(Promise::new());
    let p2 = Obj::new(Promise::new());
    let leave = Obj::new(Promise::new());
    let (s1, q1, l1) = (s.clone(), p1.clone(), leave.clone());
    let r1 = as_task(move || reader(s1, q1, l1), PRIO_DEDICATED);
    let (s2, q2, l2) = (s.clone(), p2.clone(), leave.clone());
    let r2 = as_task(move || reader(s2, q2, l2), PRIO_DEDICATED);
    p1.result_opt().get();
    p2.result_opt().get();
    let w = s.try_write();
    let r = s.try_read();
    println(&format!("two readers inside: tryWrite {w}, tryRead {r}"));
    s.unlock_read();
    let s3 = s.clone();
    let wr = as_task(
        move || {
            s3.write();
            println("writer entered");
            s3.unlock_write();
        },
        PRIO_DEDICATED,
    );
    sleep(ms);
    println("readers leave");
    leave.resolve(());
    wr.get();
    r1.get();
    r2.get();
    println(&format!("after: tryWrite {}", s.try_write()));
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let r ← BaseRecursiveMutex.new
//   r.lock
//   IO.println s!"main tryLock again: {← r.tryLock}"
//   let t ← IO.asTask (prio := .dedicated) do
//     IO.println s!"task tryLock: {← r.tryLock}"
//     r.lock
//     IO.println "task has the lock"
//     r.unlock
//   IO.sleep ms.toUInt32
//   r.unlock
//   IO.println "main unlocked once"
//   IO.sleep ms.toUInt32
//   IO.println "main unlocks the second time"
//   r.unlock
//   let _ ← IO.wait t
fn recursive_mutex(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let r = Obj::new(RecursiveMutex::new());
    r.lock();
    println(&format!("main tryLock again: {}", r.try_lock()));
    let r2 = r.clone();
    let t = as_task(
        move || {
            println(&format!("task tryLock: {}", r2.try_lock()));
            r2.lock();
            println("task has the lock");
            r2.unlock();
        },
        PRIO_DEDICATED,
    );
    sleep(ms);
    r.unlock();
    println("main unlocked once");
    sleep(ms);
    println("main unlocks the second time");
    r.unlock();
    t.get();
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let flag ← IO.mkRef false
//   let t := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   let u := t.map (· + 1)
//   let _ ← IO.mapTask (fun v => do flag.set true; IO.eprintln s!"io dependent saw {v}") u
//   while !(← flag.get) do
//     IO.sleep 10
//   IO.eprintln "main saw the flag"
fn pure_chain_io_dep(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let flag = Ref::new(false);
    let t = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    let u = map_task(|v: u64| v + 1, t, PRIO_DEFAULT, false, false);
    let f2 = flag.clone();
    let _ = map_task(
        move |v: u64| {
            f2.set(true);
            eprintln(&format!("io dependent saw {v}"));
        },
        u,
        PRIO_DEFAULT,
        false,
        true,
    );
    while !flag.get() {
        sleep(10);
    }
    eprintln("main saw the flag");
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let flag ← IO.mkRef false
//   let t := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   let u := t.bind fun v => Task.spawn fun _ => v + 1
//   let _ ← IO.mapTask (fun v => do flag.set true; IO.eprintln s!"io dependent saw {v}") u
//   while !(← flag.get) do
//     IO.sleep 10
//   IO.eprintln "main saw the flag"
fn pure_bind_io_dep(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let flag = Ref::new(false);
    let t = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    let u = bind_task(
        t,
        |v: u64| Task::spawn(move || v + 1, PRIO_DEFAULT),
        PRIO_DEFAULT,
        false,
        false,
    );
    let f2 = flag.clone();
    let _ = map_task(
        move |v: u64| {
            f2.set(true);
            eprintln(&format!("io dependent saw {v}"));
        },
        u,
        PRIO_DEFAULT,
        false,
        true,
    );
    while !flag.get() {
        sleep(10);
    }
    eprintln("main saw the flag");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   IO.println "main starts"
//   let _ ← IO.asTask (do IO.sleep 5000; IO.eprintln "other task ran")
//   let _ ← IO.asTask (do IO.sleep ms.toUInt32; IO.eprintln "task exits"; (IO.Process.exit 3 : IO Unit))
//   IO.sleep 1000
//   IO.eprintln "not reached"
fn exit_from_task(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    println("main starts");
    let _ = as_task(
        || {
            sleep(5000);
            eprintln("other task ran");
        },
        PRIO_DEFAULT,
    );
    let _ = as_task::<()>(
        move || {
            sleep(ms);
            eprintln("task exits");
            crate::glue::process_exit(3)
        },
        PRIO_DEFAULT,
    );
    sleep(1000);
    eprintln("not reached");
    0
}

// def main (args : List String) : IO Unit := do
//   let a := args[0]!.toNat!
//   let b := args[1]!.toNat!
//   let ta ← IO.asTask (IO.sleep a.toUInt32)
//   let tb ← IO.asTask (do IO.sleep b.toUInt32; return 5)
//   let d ← IO.mapTask (sync := true) (fun _ => do
//     let v ← IO.wait tb
//     IO.eprintln s!"dependent got {repr v.toOption}") ta
//   let _ ← IO.wait d
//   IO.eprintln "main done"
fn get_in_sync_task(args: &[String]) -> u32 {
    let a = to_nat(&args[0]) as u32;
    let b = to_nat(&args[1]) as u32;
    let ta = as_task(move || sleep(a), PRIO_DEFAULT);
    let tb = as_task(
        move || {
            sleep(b);
            5u64
        },
        PRIO_DEFAULT,
    );
    let d = map_task(
        move |()| {
            let v = tb.get();
            eprintln(&format!("dependent got some {v}"));
        },
        ta,
        PRIO_DEFAULT,
        true,
        true,
    );
    d.get();
    eprintln("main done");
    0
}

// ---------------------------------------------------------------------------
// sched-2: `IO.Promise.result?`, `IO.Option.getOrBlock!` (`Promise.result!`)
// and `Task.pure`.

/// `repr` of an `Option Nat`.
fn repr_opt(o: Option<u64>) -> String {
    match o {
        Some(v) => format!("some {v}"),
        None => "none".into(),
    }
}

// let ms := args.head!.toNat!
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// IO.println s!"before resolve: {← IO.getTaskState r}, finished {← IO.hasFinished p.result?}, resolved {← p.isResolved}"
// let w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait p.result?
//   IO.eprintln s!"waiter woke: {repr v}"
//   return v
// IO.sleep ms.toUInt32
// IO.println s!"waiter finished before resolve: {← IO.hasFinished w}"
// p.resolve 41
// p.resolve 42
// IO.println s!"after resolve: {← IO.getTaskState r}, {repr r.get}, again {repr (← IO.wait p.result?)}, resolved {← p.isResolved}"
// match ← IO.wait w with
// | .ok v => IO.println s!"waiter got {repr v}"
// | .error e => IO.println s!"error {e}"
// let q ← IO.Promise.new (α := Nat)
// let r2 := q.result?
// let m ← IO.mapTask (fun o => IO.eprintln s!"dependent saw {repr o}") r2
// IO.println s!"before drop: {← IO.getTaskState r2}, resolved {← q.isResolved}"
// let d := q.resultD 5
// IO.println s!"after drop: {← IO.getTaskState r2}, {repr r2.get}, resultD {d.get}"
// let _ ← IO.wait m
// IO.println "done"
fn promise_result_opt(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let p: Obj<Promise<u64>> = Obj::new(Promise::new());
    let r = p.result_opt();
    let st = task_state_str(r.state());
    let fin = has_finished(&p.result_opt());
    let res = has_finished(&p.result_opt());
    println(&format!(
        "before resolve: {st}, finished {fin}, resolved {res}"
    ));
    let p2 = p.clone();
    let w = as_task(
        move || {
            let v = p2.result_opt().get();
            eprintln(&format!("waiter woke: {}", repr_opt(v)));
            v
        },
        PRIO_DEDICATED,
    );
    sleep(ms);
    println(&format!(
        "waiter finished before resolve: {}",
        has_finished(&w)
    ));
    p.resolve(41);
    p.resolve(42);
    let st = task_state_str(r.state());
    let v = r.get();
    let again = p.result_opt().get();
    let res = has_finished(&p.result_opt());
    // `p`'s last use (resolved: its drop changes nothing).
    drop(p);
    println(&format!(
        "after resolve: {st}, {}, again {}, resolved {res}",
        repr_opt(v),
        repr_opt(again)
    ));
    println(&format!("waiter got {}", repr_opt(w.get())));
    let q: Promise<u64> = Promise::new();
    let r2 = q.result_opt();
    let m = map_task(
        |o: Option<u64>| eprintln(&format!("dependent saw {}", repr_opt(o))),
        r2.clone(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let st = task_state_str(r2.state());
    let res = has_finished(&q.result_opt());
    // `q.resultD 5` is `q.result?.map (sync := true) (·.getD 5)`, and
    // compiled Lean shares its `q.result?` with `r2`, so `q`'s last use is
    // `q.isResolved`: dropped here, before the line is printed, it resolves
    // `r2` with `none` (the walk queues `m`).
    drop(q);
    println(&format!("before drop: {st}, resolved {res}"));
    // `resultD`'s map of the finished `r2` runs at once.
    let d = map_task(
        |o: Option<u64>| o.unwrap_or(5),
        r2.clone(),
        PRIO_DEFAULT,
        true,
        false,
    );
    let st = task_state_str(r2.state());
    println(&format!(
        "after drop: {st}, {}, resultD {}",
        repr_opt(r2.get()),
        d.get()
    ));
    m.get();
    println("done");
    0
}

// let ms := args.head!.toNat!
// let p ← IO.Promise.new (α := Nat)
// p.resolve 7
// let t := p.result!
// IO.println s!"resolved first: {← IO.getTaskState t}, {t.get}"
// let q ← IO.Promise.new (α := String)
// let u := q.result!
// IO.println s!"before resolve: {← IO.getTaskState u}"
// q.resolve "from main"
// IO.println s!"after resolve: {← IO.getTaskState u}, {u.get}"
// let s ← IO.Promise.new (α := Nat)
// let v := s.result!
// let _ ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   s.resolve 99
// IO.println s!"resolved by a task: {← IO.wait v}"
fn result_bang_some(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let p: Promise<u64> = Promise::new();
    p.resolve(7);
    // Already resolved: `getOrBlock!` runs at once, and `t` is `Task.pure 7`.
    let t = p.result_bang();
    drop(p);
    println(&format!(
        "resolved first: {}, {}",
        task_state_str(t.state()),
        t.get()
    ));
    let q: Promise<String> = Promise::new();
    let u = q.result_bang();
    println(&format!("before resolve: {}", task_state_str(u.state())));
    // The `sync` dependent runs here, in `resolve`.
    q.resolve("from main".to_string());
    drop(q);
    println(&format!(
        "after resolve: {}, {}",
        task_state_str(u.state()),
        u.get()
    ));
    let s: Obj<Promise<u64>> = Obj::new(Promise::new());
    let s2 = s.clone();
    let _ = as_task(
        move || {
            sleep(ms);
            s2.resolve(99);
        },
        PRIO_DEDICATED,
    );
    // Compiled Lean makes the pure `s.result!` where it is first used, after
    // the task.
    let v = s.result_bang();
    drop(s);
    println(&format!("resolved by a task: {}", v.get()));
    0
}

// IO.println "before"
// let p ← IO.Promise.new (α := Nat)
// let t := p.result!
// IO.eprintln s!"not reached: {← IO.hasFinished t}"
fn result_bang_dropped(_: &[String]) -> u32 {
    println("before");
    let p: Promise<u64> = Promise::new();
    let t = p.result_bang();
    // `p`'s last use: the drop resolves it with `none`, and the `sync`
    // dependent reports the panic and waits forever, here on `main`.
    drop(p);
    eprintln(&format!("not reached: {}", has_finished(&t)));
    0
}

// let ms := args.head!.toNat!
// IO.println "before"
// let t ← IO.asTask (prio := .dedicated) do
//   let p ← IO.Promise.new (α := Nat)
//   let t := p.result!
//   IO.eprintln s!"not reached: {← IO.hasFinished t}"
// IO.sleep ms.toUInt32
// IO.eprintln s!"main: dropping task finished: {← IO.hasFinished t}"
// let o ← IO.asTask (IO.eprintln "another task runs")
// let _ ← IO.wait o
// IO.println "main done"
// IO.eprintln "main returns"
fn result_bang_dropped_in_task(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    println("before");
    let t = as_task(
        || {
            let p: Promise<u64> = Promise::new();
            let t = p.result_bang();
            drop(p);
            eprintln(&format!("not reached: {}", has_finished(&t)));
        },
        PRIO_DEDICATED,
    );
    sleep(ms);
    eprintln(&format!(
        "main: dropping task finished: {}",
        has_finished(&t)
    ));
    let o = as_task(|| eprintln("another task runs"), PRIO_DEFAULT);
    o.get();
    println("main done");
    eprintln("main returns");
    0
}

// IO.println "before"
// let buf ← IO.mkRef {}
// let _ ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
// let p ← IO.Promise.new (α := Nat)
// let t := p.result!
// IO.eprintln s!"not reached: {← IO.hasFinished t}"
fn result_bang_dropped_abort(_: &[String]) -> u32 {
    println("before");
    // Under `LEAN_ABORT_ON_PANIC` every Lean panic goes to the process's
    // stderr, the buffer or not (`result_bang_dropped_redirected` tells the
    // two apart).
    let _ = set_stderr_stream(Some(Obj::new(Var::new(Vec::new()))));
    let p: Promise<u64> = Promise::new();
    let t = p.result_bang();
    drop(p);
    eprintln(&format!("not reached: {}", has_finished(&t)));
    0
}

// let p ← IO.Promise.new (α := Nat)
// let t := p.result!
// IO.println s!"result! pending: {← IO.getTaskState t}"
// IO.println s!"promise resolved: {← p.isResolved}"
// IO.println "no panic"
fn result_bang_dropped_first(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let t = p.result_bang();
    let st = task_state_str(t.state());
    // `t`'s last use: the dependent is deleted (`release`).
    drop(t);
    println(&format!("result! pending: {st}"));
    let res = has_finished(&p.result_opt());
    // `p`'s last use: resolved with `none`, no dependent left.
    drop(p);
    println(&format!("promise resolved: {res}"));
    println("no panic");
    0
}

// let n := args[0]!.toNat!
// let ms := args[1]!.toNat!
// let t := Task.pure n
// IO.println s!"pure: {← IO.getTaskState t}, {t.get}"
// IO.cancel t
// IO.println s!"after cancel: {← IO.getTaskState t}, {← IO.wait t}"
// let m1 := t.map (· + 1)
// let m2 ← IO.mapTask (sync := true) (fun x => do
//   IO.println s!"sync dependent of a pure task runs at once: {x}"
//   return x * 2) t
// IO.println s!"after the sync dependent: {← IO.getTaskState m2}"
// let b1 := t.bind fun x => Task.pure (x + 10)
// let b2 ← IO.bindTask t fun x => return Task.pure (.ok (x + 20))
// let slow ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   return n + 30
// let b3 ← IO.bindTask slow fun r => return Task.pure (r.map (· + 1))
// let b4 := (Task.spawn fun _ => n + 40).bind fun x => Task.pure (x + 1)
// let any ← IO.waitAny [slow.map (fun r => r.toOption.getD 0), Task.pure (n + 50)]
// IO.println s!"waitAny: {any}, slow finished: {← IO.hasFinished slow}"
// let inner ← IO.asTask do
//   let v ← (← IO.mkRef (n + 60)).get
//   let p := Task.pure v
//   let q ← IO.mapTask (sync := true) (fun x => do
//     IO.eprintln s!"inner sync dependent of a pure task: {x}"
//     return x + 1) p
//   return (← IO.wait q).toOption.getD 0
// IO.println s!"m1 {m1.get}, m2 {repr (← IO.wait m2).toOption}, b1 {b1.get}, b2 {repr (← IO.wait b2).toOption}"
// IO.println s!"b3 {repr (← IO.wait b3).toOption}, b4 {b4.get}, inner {repr (← IO.wait inner).toOption}"
fn task_pure_graph(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let ms = to_nat(&args[1]) as u32;
    let t = Task::pure(n);
    println(&format!("pure: {}, {}", task_state_str(t.state()), t.get()));
    cancel(&t);
    println(&format!(
        "after cancel: {}, {}",
        task_state_str(t.state()),
        t.get()
    ));
    let m1 = map_task(|x: u64| x + 1, t.clone(), PRIO_DEFAULT, false, false);
    let m2 = map_task(
        |x: u64| {
            println(&format!("sync dependent of a pure task runs at once: {x}"));
            x * 2
        },
        t.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    println(&format!(
        "after the sync dependent: {}",
        task_state_str(m2.state())
    ));
    let b1 = bind_task(
        t.clone(),
        |x: u64| Task::pure(x + 10),
        PRIO_DEFAULT,
        false,
        false,
    );
    let b2 = bind_task(t, |x: u64| Task::pure(x + 20), PRIO_DEFAULT, false, true);
    let slow = as_task(
        move || {
            sleep(ms);
            n + 30
        },
        PRIO_DEDICATED,
    );
    let b3 = bind_task(
        slow.clone(),
        |x: u64| Task::pure(x + 1),
        PRIO_DEFAULT,
        false,
        true,
    );
    let b4 = bind_task(
        Task::spawn(move || n + 40, PRIO_DEFAULT),
        |x: u64| Task::pure(x + 1),
        PRIO_DEFAULT,
        false,
        false,
    );
    let any = wait_any(&[
        map_task(|x: u64| x, slow.clone(), PRIO_DEFAULT, false, false),
        Task::pure(n + 50),
    ]);
    println(&format!(
        "waitAny: {any}, slow finished: {}",
        has_finished(&slow)
    ));
    let inner = as_task(
        move || {
            let v = Ref::new(n + 60).get();
            let p = Task::pure(v);
            let q = map_task(
                |x: u64| {
                    eprintln(&format!("inner sync dependent of a pure task: {x}"));
                    x + 1
                },
                p,
                PRIO_DEFAULT,
                true,
                true,
            );
            q.get()
        },
        PRIO_DEFAULT,
    );
    println(&format!(
        "m1 {}, m2 some {}, b1 {}, b2 some {}",
        m1.get(),
        m2.get(),
        b1.get(),
        b2.get()
    ));
    println(&format!(
        "b3 some {}, b4 {}, inner some {}",
        b3.get(),
        b4.get(),
        inner.get()
    ));
    0
}

/// Lean's current stderr as the twins set it: a buffer
/// (`IO.FS.Stream.ofBuffer`), or `None` for the process's stream.
type ErrStream = Option<Obj<Var<Vec<u8>>>>;

/// `IO.setStderr s`: the runtime's lines written with `io_eprintln` now go
/// to `s` (`io::streams::set_stderr`, with `s`'s `putStr`); returns the
/// previous stream.
fn set_stderr_stream(s: ErrStream) -> ErrStream {
    // `StderrPut` is an `Rc` in both modes: it stays on the thread that set
    // the stream.
    let put: lean_runtime::io::streams::StderrPut = match &s {
        Some(b) => {
            let b = b.clone();
            std::rc::Rc::new(move |l: &[u8]| b.borrow_mut().extend_from_slice(l))
        }
        None => std::rc::Rc::new(|l: &[u8]| {
            let _ = lean_runtime::io::Handle::stderr().put_str(l);
        }),
    };
    lean_runtime::io::streams::set_stderr(s, put, || None)
}

// IO.println "before"
// let buf ← IO.mkRef {}
// let _ ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
// let p ← IO.Promise.new (α := Nat)
// let t := p.result!
// IO.eprintln s!"not reached: {← IO.hasFinished t}"
fn result_bang_dropped_redirected(_: &[String]) -> u32 {
    println("before");
    // The forced panic bypasses the buffer: a report through Lean's current
    // stderr would leave the process's stderr empty.
    let _ = set_stderr_stream(Some(Obj::new(Var::new(Vec::new()))));
    let p: Promise<u64> = Promise::new();
    let t = p.result_bang();
    drop(p);
    eprintln(&format!("not reached: {}", has_finished(&t)));
    0
}

// let ms := args.head!.toNat!
// let buf ← IO.mkRef {}
// let old ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
// let p ← IO.Promise.new (α := Unit)
// let tb ← IO.asTask (do IO.sleep ms.toUInt32; return 5)
// let d ← IO.mapTask (sync := true) (fun _ => do
//   let v ← IO.wait tb
//   IO.println s!"dependent got {repr v.toOption}") p.result?
// p.resolve ()
// let _ ← IO.wait d
// let _ ← IO.setStderr old
// let b ← buf.get
// IO.println s!"captured: {repr (String.fromUTF8! b.data)}"
// IO.eprintln "main done"
fn get_in_sync_task_redirected(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let buf = Obj::new(Var::new(Vec::new()));
    let old = set_stderr_stream(Some(buf.clone()));
    let p: Promise<()> = Promise::new();
    let tb = as_task(
        move || {
            sleep(ms);
            5u64
        },
        PRIO_DEFAULT,
    );
    // `main` resolves `p`, so the dependent runs on `main`'s context, and
    // its panic goes to the buffer.
    let d = map_task(
        move |_: Option<()>| {
            let v = tb.get();
            println(&format!("dependent got some {v}"));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    p.resolve(());
    d.get();
    let _ = set_stderr_stream(old);
    let b = buf.borrow().clone();
    println(&format!(
        "captured: {}",
        crate::lio::quote(&String::from_utf8(b).expect("UTF-8"))
    ));
    eprintln("main done");
    0
}

// let p ← IO.Promise.new (α := Nat)
// IO.cancel p.result?
// let d ← IO.mapTask (fun _ => IO.checkCanceled) p.result?
// let s ← IO.mapTask (sync := true) (fun _ => IO.checkCanceled) p.result?
// IO.println s!"state after cancel: {← IO.getTaskState p.result?}"
// let a ← IO.waitAny [p.result?, Task.pure (some 3)]
// IO.println s!"waitAny: {repr a}"
// p.resolve 1
// IO.println s!"canceled promise deps: {← IO.wait d} {← IO.wait s}"
// let t := Task.pure 1
// IO.cancel t
// let d2 ← IO.mapTask (fun _ => IO.checkCanceled) t
// IO.println s!"pure dep: {← IO.wait d2}"
fn cancel_promise_and_pure(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    cancel(&p.result_opt());
    let d = map_task(
        |_: Option<u64>| check_canceled(),
        p.result_opt(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let s = map_task(
        |_: Option<u64>| check_canceled(),
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    println(&format!(
        "state after cancel: {}",
        task_state_str(p.result_opt().state())
    ));
    let a = wait_any(&[p.result_opt(), Task::pure(Some(3))]);
    println(&format!("waitAny: {}", repr_opt(a)));
    p.resolve(1);
    println(&format!(
        "canceled promise deps: ok: {} ok: {}",
        d.get(),
        s.get()
    ));
    let t = Task::pure(1u64);
    cancel(&t);
    let d2 = map_task(|_: u64| check_canceled(), t, PRIO_DEFAULT, false, true);
    println(&format!("pure dep: ok: {}", d2.get()));
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let _before ← IO.mapTask (fun o => IO.eprintln s!"dep before: {repr o}") r
// let t := p.result!
// IO.eprintln s!"t: {← IO.getTaskState t}"
// let _after ← IO.mapTask (fun o => IO.eprintln s!"dep after: {repr o}") r
// let ref ← IO.mkRef (some p)
// IO.eprintln "dropping"
// ref.set none
// IO.eprintln s!"not reached {← IO.hasFinished t}"
fn result_bang_dep_order(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    // `_before` and `_after` are unused: compiled Lean drops them at once.
    drop(map_task(
        |o: Option<u64>| eprintln(&format!("dep before: {}", repr_opt(o))),
        r.clone(),
        PRIO_DEFAULT,
        false,
        true,
    ));
    let t = p.result_bang();
    eprintln(&format!("t: {}", task_state_str(t.state())));
    drop(map_task(
        |o: Option<u64>| eprintln(&format!("dep after: {}", repr_opt(o))),
        r,
        PRIO_DEFAULT,
        false,
        true,
    ));
    let rf = Ref::new(Some(Obj::new(p)));
    eprintln("dropping");
    // The old value, the promise's last reference, is dropped in `set`: the
    // walk queues `_after`, then `result!` hangs, and `_before` is never
    // reached.
    rf.set(None);
    eprintln(&format!("not reached {}", has_finished(&t)));
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r ← IO.mkRef 5
// let pt := Task.pure (← r.get)
// let s ← IO.mapTask (sync := true) (fun o => do
//   let v ← IO.wait pt
//   let u ← IO.mkRef (v + 1)
//   let w := Task.pure (← u.get)
//   IO.eprintln s!"sync dependent: {repr o}, {v}, {w.get}") p.result?
// IO.eprintln "resolving"
// p.resolve 1
// let _ ← IO.wait s
// IO.eprintln "done"
fn pure_get_in_sync_task(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = Ref::new(5u64);
    let pt = Task::pure(r.get());
    let s = map_task(
        move |o: Option<u64>| {
            // The slot holds the value: no `GET_IN_SYNC_TASK`.
            let v = pt.get();
            let u = Ref::new(v + 1);
            let w = Task::pure(u.get());
            eprintln(&format!(
                "sync dependent: {}, {v}, {}",
                repr_opt(o),
                w.get()
            ));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    eprintln("resolving");
    p.resolve(1);
    s.get();
    eprintln("done");
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let s ← IO.mapTask (sync := true) (fun o => do IO.sleep 300; IO.eprintln s!"sync dep done {repr o}") r
// let w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait r
//   return s!"waiter woke: {repr v}, sync dep finished: {← IO.hasFinished s}"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// match ← IO.wait w with
// | .ok m => IO.eprintln m
// | .error e => IO.eprintln s!"waiter: {e}"
fn sync_dependent_before_waiter(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let s = map_task(
        |o: Option<u64>| {
            sleep(300);
            eprintln(&format!("sync dep done {}", repr_opt(o)));
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let w = as_task(
        move || {
            let v = r.get();
            format!(
                "waiter woke: {}, sync dep finished: {}",
                repr_opt(v),
                has_finished(&s)
            )
        },
        PRIO_DEDICATED,
    );
    sleep(100);
    eprintln("resolving");
    // The waiter wakes once the walk, and the sleeping `sync` dependent in
    // it, is over.
    p.resolve(1);
    eprintln("resolved");
    eprintln(&w.get());
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let t := p.result!
// let _w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait r
//   IO.eprintln s!"waiter woke: {repr v}"
// let ref ← IO.mkRef (some p)
// IO.sleep 200
// IO.eprintln "dropping"
// ref.set none
// IO.eprintln s!"not reached {← IO.hasFinished t}"
fn dropped_promise_waiter_wakes(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    // `_w` is unused: compiled Lean drops it at once.
    drop(as_task(
        move || {
            let v = r.get();
            eprintln(&format!("waiter woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    ));
    // Compiled Lean makes the pure `p.result!` after the task.
    let t = p.result_bang();
    let rf = Ref::new(Some(Obj::new(p)));
    sleep(200);
    eprintln("dropping");
    // LB-32: `result!` reaches its permanent block in the drop's walk; the
    // waiter of `result?` wakes then, with `none`.
    rf.set(None);
    eprintln(&format!("not reached {}", has_finished(&t)));
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let t := p.result!
// let _w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait r
//   IO.eprintln s!"waiter woke: {repr v}"
// let o ← IO.asTask (prio := .dedicated) do
//   IO.sleep 1000
//   IO.eprintln "other task finishes"
// let keep ← IO.mkRef [o]
// let ref ← IO.mkRef (some p)
// IO.sleep 200
// IO.eprintln "dropping"
// ref.set none
// IO.eprintln s!"not reached {← IO.hasFinished t} {(← keep.get).length}"
fn dropped_promise_waiter_unrelated_finish(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    // `_w` is unused: compiled Lean drops it at once.
    drop(as_task(
        move || {
            let v = r.get();
            eprintln(&format!("waiter woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    ));
    let o = as_task(
        || {
            sleep(1000);
            eprintln("other task finishes");
        },
        PRIO_DEDICATED,
    );
    let keep = Ref::new(vec![o]);
    // Compiled Lean makes the pure `p.result!` here.
    let t = p.result_bang();
    let rf = Ref::new(Some(Obj::new(p)));
    sleep(200);
    eprintln("dropping");
    rf.set(None);
    eprintln(&format!(
        "not reached {} {}",
        has_finished(&t),
        keep.get().len()
    ));
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let slow ← IO.mapTask (sync := true) (fun o => do IO.sleep 300; IO.eprintln s!"slow sync dep done {repr o}") r
// let quick ← IO.mapTask (sync := true) (fun o => IO.eprintln s!"quick sync dep done {repr o}") r
// let w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait r
//   IO.eprintln s!"waiter woke: {repr v}"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// let _ ← IO.wait w
// let _ ← IO.wait slow
// let _ ← IO.wait quick
fn waiter_wakes_after_nested_finish(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let slow = map_task(
        |o: Option<u64>| {
            sleep(300);
            eprintln(&format!("slow sync dep done {}", repr_opt(o)));
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let quick = map_task(
        |o: Option<u64>| eprintln(&format!("quick sync dep done {}", repr_opt(o))),
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let w = as_task(
        move || {
            let v = r.get();
            eprintln(&format!("waiter woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    );
    sleep(100);
    eprintln("resolving");
    // The walk runs `quick` (newest first), whose finish wakes the waiter,
    // then sleeps in `slow`.
    p.resolve(1);
    eprintln("resolved");
    w.get();
    slow.get();
    quick.get();
    0
}

// partial def spin : IO Unit := do
//   IO.sleep 100
//   spin
//
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let s ← IO.mapTask (sync := true) (fun _ => spin) r
// let _w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait r
//   IO.eprintln s!"waiter woke: {repr v}"
// let o ← IO.asTask (prio := .dedicated) do
//   IO.sleep 400
//   IO.eprintln "other task finishes"
// let keep ← IO.mkRef [o]
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln s!"not reached {← IO.hasFinished s} {(← keep.get).length}"
fn sync_walk_stuck_unrelated_finish(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let s = map_task(
        |_: Option<u64>| loop {
            sleep(100);
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    // `_w` is unused: compiled Lean drops it at once.
    drop(as_task(
        move || {
            let v = r.get();
            eprintln(&format!("waiter woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    ));
    let o = as_task(
        || {
            sleep(400);
            eprintln("other task finishes");
        },
        PRIO_DEDICATED,
    );
    let keep = Ref::new(vec![o]);
    sleep(100);
    eprintln("resolving");
    // The walk never ends; `o`'s finish wakes the waiter.
    p.resolve(1);
    eprintln(&format!(
        "not reached {} {}",
        has_finished(&s),
        keep.get().len()
    ));
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let slow ← IO.mapTask (sync := true) (fun _ => do IO.sleep 300; IO.eprintln "slow sync dep done") r
// let w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.waitAny [r]
//   IO.eprintln s!"waitAny woke: {repr v}"
// let a ← IO.mapTask (fun _ => do IO.sleep 100; IO.eprintln "async dep done") r
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// let _ ← IO.wait w
// let _ ← IO.wait slow
// let _ ← IO.wait a
fn wait_any_wakes_on_finish(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let slow = map_task(
        |_: Option<u64>| {
            sleep(300);
            eprintln("slow sync dep done");
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let r2 = r.clone();
    let w = as_task(
        move || {
            let v = wait_any(&[r2]);
            eprintln(&format!("waitAny woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    );
    let a = map_task(
        |_: Option<u64>| {
            sleep(100);
            eprintln("async dep done");
        },
        r,
        PRIO_DEFAULT,
        false,
        true,
    );
    sleep(100);
    eprintln("resolving");
    // The walk queues `a` (no wake for `waitAny`), then sleeps in `slow`;
    // `a`'s finish wakes `waitAny`.
    p.resolve(1);
    eprintln("resolved");
    w.get();
    slow.get();
    a.get();
    0
}

// let m ← Std.Mutex.new (0 : Nat)
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let s ← IO.mapTask (sync := true) (fun _ => do
//   m.atomically (modify (· + 1))
//   IO.eprintln "sync dep got the mutex") r
// let w ← IO.asTask (prio := .dedicated) do
//   m.atomically do
//     let v ← IO.wait r
//     IO.eprintln s!"waiter woke holding the mutex: {repr v}"
// let o ← IO.asTask (prio := .dedicated) do
//   IO.sleep 400
//   IO.eprintln "other task finishes"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// let _ ← IO.wait w
// let _ ← IO.wait s
// let _ ← IO.wait o
// IO.eprintln "done"
fn sync_walk_mutex_unrelated_finish(_: &[String]) -> u32 {
    let m = Obj::new(Mutex::new());
    let count = Ref::new(0u64);
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let (m1, c1) = (m.clone(), count.clone());
    let s = map_task(
        move |_: Option<u64>| {
            m1.lock();
            c1.modify(|n| n + 1);
            m1.unlock();
            eprintln("sync dep got the mutex");
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let m2 = m.clone();
    let w = as_task(
        move || {
            m2.lock();
            let v = r.get();
            eprintln(&format!("waiter woke holding the mutex: {}", repr_opt(v)));
            m2.unlock();
        },
        PRIO_DEDICATED,
    );
    let o = as_task(
        || {
            sleep(400);
            eprintln("other task finishes");
        },
        PRIO_DEDICATED,
    );
    sleep(100);
    eprintln("resolving");
    // The walk blocks in `s` on the mutex the waiter holds; `o`'s finish
    // wakes the waiter, which releases it.
    p.resolve(1);
    eprintln("resolved");
    w.get();
    s.get();
    o.get();
    eprintln("done");
    0
}

// `sync_walk_mutex_unrelated_finish` without the unrelated task: a deadlock,
// natively and here (documented misuse, not LB-32).
//
// let m ← Std.Mutex.new (0 : Nat)
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let s ← IO.mapTask (sync := true) (fun _ => do
//   m.atomically (modify (· + 1))
//   IO.eprintln "sync dep got the mutex") r
// let w ← IO.asTask (prio := .dedicated) do
//   m.atomically do
//     let v ← IO.wait r
//     IO.eprintln s!"waiter woke holding the mutex: {repr v}"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// let _ ← IO.wait w
// let _ ← IO.wait s
// IO.eprintln "done"
fn sync_walk_mutex_alone(_: &[String]) -> u32 {
    let m = Obj::new(Mutex::new());
    let count = Ref::new(0u64);
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let (m1, c1) = (m.clone(), count.clone());
    let s = map_task(
        move |_: Option<u64>| {
            m1.lock();
            c1.modify(|n| n + 1);
            m1.unlock();
            eprintln("sync dep got the mutex");
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let m2 = m.clone();
    let w = as_task(
        move || {
            m2.lock();
            let v = r.get();
            eprintln(&format!("waiter woke holding the mutex: {}", repr_opt(v)));
            m2.unlock();
        },
        PRIO_DEDICATED,
    );
    sleep(100);
    eprintln("resolving");
    // The walk blocks in `s` on the mutex the waiter holds, and no other
    // task ever finishes to wake the waiter.
    p.resolve(1);
    eprintln("resolved");
    w.get();
    s.get();
    eprintln("done");
    0
}

// `sync_walk_stuck_unrelated_finish` without the unrelated task: the waiter
// never wakes, natively and here (documented misuse, not LB-32).
//
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let s ← IO.mapTask (sync := true) (fun _ => spin) r
// let _w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.wait r
//   IO.eprintln s!"waiter woke: {repr v}"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln s!"not reached {← IO.hasFinished s}"
fn sync_walk_stuck_alone(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let s = map_task(
        |_: Option<u64>| loop {
            sleep(100);
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    // `_w` is unused: compiled Lean drops it at once.
    drop(as_task(
        move || {
            let v = r.get();
            eprintln(&format!("waiter woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    ));
    sleep(100);
    eprintln("resolving");
    p.resolve(1);
    eprintln(&format!("not reached {}", has_finished(&s)));
    0
}

// `sync_walk_mutex_unrelated_finish` with the unrelated task unreferenced:
// its finish notifies nobody (natively `m_deleted`), a deadlock natively and
// here (documented misuse, not LB-32).
//
// let m ← Std.Mutex.new (0 : Nat)
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let s ← IO.mapTask (sync := true) (fun _ => do
//   m.atomically (modify (· + 1))
//   IO.eprintln "sync dep got the mutex") r
// let w ← IO.asTask (prio := .dedicated) do
//   m.atomically do
//     let v ← IO.wait r
//     IO.eprintln s!"waiter woke holding the mutex: {repr v}"
// let _ ← IO.asTask (prio := .dedicated) do
//   IO.sleep 400
//   IO.eprintln "other task finishes"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// let _ ← IO.wait w
// let _ ← IO.wait s
// IO.eprintln "done"
fn sync_walk_mutex_unref_finish(_: &[String]) -> u32 {
    let m = Obj::new(Mutex::new());
    let count = Ref::new(0u64);
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let (m1, c1) = (m.clone(), count.clone());
    let s = map_task(
        move |_: Option<u64>| {
            m1.lock();
            c1.modify(|n| n + 1);
            m1.unlock();
            eprintln("sync dep got the mutex");
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let m2 = m.clone();
    let w = as_task(
        move || {
            m2.lock();
            let v = r.get();
            eprintln(&format!("waiter woke holding the mutex: {}", repr_opt(v)));
            m2.unlock();
        },
        PRIO_DEDICATED,
    );
    // `let _ ← IO.asTask`: the handle is dropped at once (`release`).
    drop(as_task(
        || {
            sleep(400);
            eprintln("other task finishes");
        },
        PRIO_DEDICATED,
    ));
    sleep(100);
    eprintln("resolving");
    p.resolve(1);
    eprintln("resolved");
    w.get();
    s.get();
    eprintln("done");
    0
}

// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let slow ← IO.mapTask (sync := true) (fun _ => do IO.sleep 600; IO.eprintln "slow sync dep done") r
// let w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.waitAny [r]
//   IO.eprintln s!"waitAny woke: {repr v}"
// IO.sleep 100
// let _ ← IO.asTask (prio := .dedicated) do
//   IO.sleep 200
//   IO.eprintln "unreferenced task finishes"
// let o ← IO.asTask (prio := .dedicated) do
//   IO.sleep 400
//   IO.eprintln "referenced task finishes"
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln "resolved"
// let _ ← IO.wait w
// let _ ← IO.wait slow
// let _ ← IO.wait o
fn wait_any_unref_finish(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let slow = map_task(
        |_: Option<u64>| {
            sleep(600);
            eprintln("slow sync dep done");
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let w = as_task(
        move || {
            let v = wait_any(&[r]);
            eprintln(&format!("waitAny woke: {}", repr_opt(v)));
        },
        PRIO_DEDICATED,
    );
    sleep(100);
    // Unreferenced: its finish notifies nobody.
    drop(as_task(
        || {
            sleep(200);
            eprintln("unreferenced task finishes");
        },
        PRIO_DEDICATED,
    ));
    let o = as_task(
        || {
            sleep(400);
            eprintln("referenced task finishes");
        },
        PRIO_DEDICATED,
    );
    eprintln("resolving");
    p.resolve(1);
    eprintln("resolved");
    w.get();
    slow.get();
    o.get();
    0
}

// partial def spin : IO Unit := do
//   IO.sleep 100
//   spin
//
// let p ← IO.Promise.new (α := Nat)
// let r := p.result?
// let _s ← IO.mapTask (sync := true) (fun _ => spin) r
// let x := r.map (fun o => o.getD 0 + 1)
// let w ← IO.asTask (prio := .dedicated) do
//   let v ← IO.waitAny [x]
//   IO.eprintln s!"waitAny woke: {v}"
// IO.sleep 100
// IO.eprintln "resolving"
// p.resolve 1
// IO.eprintln s!"not reached {(← IO.wait w).isOk}"
fn wait_any_pure_stalled(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    // `_s` is unused: compiled Lean drops it at once.
    drop(map_task(
        |_: Option<u64>| loop {
            sleep(100);
        },
        r.clone(),
        PRIO_DEFAULT,
        true,
        true,
    ));
    let x = map_task(
        |o: Option<u64>| o.unwrap_or(0) + 1,
        r,
        PRIO_DEFAULT,
        false,
        false,
    );
    let w = as_task(
        move || {
            let v = wait_any(&[x]);
            eprintln(&format!("waitAny woke: {v}"));
        },
        PRIO_DEDICATED,
    );
    sleep(100);
    eprintln("resolving");
    // The walk queues `x` (which wakes `waitAny`, which runs it), then
    // stalls in the endless `sync` dependent.
    p.resolve(1);
    eprintln(&format!("not reached {}", has_finished(&w)));
    0
}

// ---------------------------------------------------------------------------
// LB-13: tasks enqueued after `main` returned. The twins give the correct
// outcome, which the cases expect (native's is in their `native` field).

// let _ ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   IO.println "dedicated enqueues the late task"
//   let _ ← IO.asTask (IO.println "late task ran")
// IO.eprintln "main returns"
fn late_task_after_main(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            println("dedicated enqueues the late task");
            let _ = as_task(|| println("late task ran"), PRIO_DEFAULT);
        },
        PRIO_DEDICATED,
    );
    eprintln("main returns");
    0
}

// let d ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   IO.eprintln "dedicated done"
// let _ ← IO.mapTask (fun _ => IO.eprintln "dependent ran") d
// IO.eprintln "main returns"
fn late_dependent_of_dedicated(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let d = as_task(
        move || {
            sleep(ms);
            eprintln("dedicated done");
        },
        PRIO_DEDICATED,
    );
    let _ = map_task(|()| eprintln("dependent ran"), d, PRIO_DEFAULT, false, true);
    eprintln("main returns");
    0
}

// let _ ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   let t ← IO.asTask (pure 5)
//   let v ← IO.wait t
//   IO.eprintln s!"dedicated got {repr v.toOption}"
// IO.eprintln "main returns"
fn late_wait_dedicated(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            let t = as_task(|| 5u64, PRIO_DEFAULT);
            eprintln(&format!("dedicated got some {}", t.get()));
        },
        PRIO_DEDICATED,
    );
    eprintln("main returns");
    0
}

// let _ ← IO.asTask do
//   IO.sleep ms.toUInt32
//   let t ← IO.asTask (pure 5)
//   let v ← IO.wait t
//   IO.eprintln s!"task got {repr v.toOption}"
// IO.eprintln "main returns"
fn late_wait_pool(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            let t = as_task(|| 5u64, PRIO_DEFAULT);
            eprintln(&format!("task got some {}", t.get()));
        },
        PRIO_DEFAULT,
    );
    eprintln("main returns");
    0
}

// let _ ← IO.asTask do
//   IO.sleep ms.toUInt32
//   let _ ← IO.asTask (IO.eprintln "late task ran")
//   IO.eprintln "task done"
// IO.eprintln "main returns"
fn late_pool_child_runs(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            let _ = as_task(|| eprintln("late task ran"), PRIO_DEFAULT);
            eprintln("task done");
        },
        PRIO_DEFAULT,
    );
    eprintln("main returns");
    0
}

// let _ ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   let _ ← IO.asTask (prio := .dedicated) (IO.eprintln "late dedicated ran")
//   IO.eprintln "dedicated done"
// IO.eprintln "main returns"
fn late_dedicated_child_runs(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let _ = as_task(
        move || {
            sleep(ms);
            let _ = as_task(|| eprintln("late dedicated ran"), PRIO_DEDICATED);
            eprintln("dedicated done");
        },
        PRIO_DEDICATED,
    );
    eprintln("main returns");
    0
}

// let d ← IO.asTask (prio := .dedicated) do
//   IO.sleep ms.toUInt32
//   let t ← IO.asTask (pure 5)
//   let v ← IO.wait t
//   IO.eprintln s!"dedicated got {repr v.toOption}"
// let _ ← IO.wait d
// IO.eprintln "main returns"
fn main_waits_dedicated_child(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let d = as_task(
        move || {
            sleep(ms);
            let t = as_task(|| 5u64, PRIO_DEFAULT);
            eprintln(&format!("dedicated got some {}", t.get()));
        },
        PRIO_DEDICATED,
    );
    d.get();
    eprintln("main returns");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.map String.toNat!
//   let b ← IO.asTask (IO.sleep ms[0]!.toUInt32)
//   let a ← IO.mapTask (fun _ => IO.eprintln "async dep ran") b
//   let _ ← IO.mapTask (sync := true) (fun _ => do
//     IO.sleep ms[2]!.toUInt32
//     IO.eprintln "sync dep done") b
//   IO.sleep ms[1]!.toUInt32
//   let _ ← IO.wait a
//   IO.eprintln "main got a"
fn wait_dep_mid_walk(args: &[String]) -> u32 {
    let ms: Vec<u32> = args.iter().map(|a| to_nat(a) as u32).collect();
    let (m0, m2) = (ms[0], ms[2]);
    let b = as_task(move || sleep(m0), PRIO_DEFAULT);
    let a = map_task(
        |()| eprintln("async dep ran"),
        b.clone(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let _ = map_task(
        move |()| {
            sleep(m2);
            eprintln("sync dep done");
        },
        b,
        PRIO_DEFAULT,
        true,
        true,
    );
    sleep(ms[1]);
    a.get();
    eprintln("main got a");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.map String.toNat!
//   let p ← IO.Promise.new (α := Unit)
//   let a ← IO.mapTask (fun _ => IO.eprintln "async dep ran") p.result?
//   let _ ← IO.mapTask (sync := true) (fun _ => do
//     IO.sleep ms[2]!.toUInt32
//     IO.eprintln "sync dep done") p.result?
//   let _ ← IO.asTask (do
//     IO.sleep ms[0]!.toUInt32
//     p.resolve ())
//   IO.sleep ms[1]!.toUInt32
//   let _ ← IO.wait a
//   IO.eprintln "main got a"
fn wait_dep_mid_promise_walk(args: &[String]) -> u32 {
    let ms: Vec<u32> = args.iter().map(|a| to_nat(a) as u32).collect();
    let (m0, m2) = (ms[0], ms[2]);
    let p: Promise<()> = Promise::new();
    let a = map_task(
        |_: Option<()>| eprintln("async dep ran"),
        p.result_opt(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let _ = map_task(
        move |_: Option<()>| {
            sleep(m2);
            eprintln("sync dep done");
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let _ = as_task(
        move || {
            sleep(m0);
            p.resolve(());
        },
        PRIO_DEFAULT,
    );
    sleep(ms[1]);
    a.get();
    eprintln("main got a");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.map String.toNat!
//   let b ← IO.asTask (IO.sleep ms[0]!.toUInt32)
//   let a ← IO.mapTask (fun _ => IO.eprintln "a ran") b
//   let _ ← IO.mapTask (sync := true) (fun _ => do
//     IO.sleep ms[2]!.toUInt32
//     IO.eprintln "sync dep of b done") b
//   IO.sleep ms[1]!.toUInt32
//   let _ ← IO.waitAny [a]
//   IO.eprintln "main got a"
//   let b2 ← IO.asTask (IO.sleep ms[0]!.toUInt32)
//   let c ← IO.mapTask (fun _ => IO.eprintln "c ran") b2
//   let _ ← IO.mapTask (sync := true) (fun _ => do
//     IO.sleep ms[2]!.toUInt32
//     IO.eprintln "sync dep of b2 done") b2
//   IO.sleep ms[1]!.toUInt32
//   while !(← IO.hasFinished c) do
//     IO.sleep 5
//   IO.eprintln "main saw c finished"
fn poll_dep_mid_walk(args: &[String]) -> u32 {
    let ms: Vec<u32> = args.iter().map(|a| to_nat(a) as u32).collect();
    let (m0, m2) = (ms[0], ms[2]);
    let b = as_task(move || sleep(m0), PRIO_DEFAULT);
    let a = map_task(|()| eprintln("a ran"), b.clone(), PRIO_DEFAULT, false, true);
    let _ = map_task(
        move |()| {
            sleep(m2);
            eprintln("sync dep of b done");
        },
        b,
        PRIO_DEFAULT,
        true,
        true,
    );
    sleep(ms[1]);
    wait_any(&[a]);
    eprintln("main got a");
    let b2 = as_task(move || sleep(m0), PRIO_DEFAULT);
    let c = map_task(
        |()| eprintln("c ran"),
        b2.clone(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let _ = map_task(
        move |()| {
            sleep(m2);
            eprintln("sync dep of b2 done");
        },
        b2,
        PRIO_DEFAULT,
        true,
        true,
    );
    sleep(ms[1]);
    while !has_finished(&c) {
        sleep(5);
    }
    eprintln("main saw c finished");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.map String.toNat!
//   let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
//   let t ← IO.asTask (do
//     IO.sleep ms[0]!.toUInt32
//     let some x ← r.get | IO.eprintln "no dependent"
//     let y ← IO.asTask (IO.sleep ms[1]!.toUInt32)
//     let _ ← IO.waitAny [x, y]
//     IO.eprintln "task's waitAny returned")
//   let x ← IO.mapTask (fun _ => IO.eprintln "dependent ran") t
//   r.set (some x)
//   let _ ← IO.wait x
//   IO.eprintln "main done"
fn wait_any_own_dep(args: &[String]) -> u32 {
    let ms: Vec<u32> = args.iter().map(|a| to_nat(a) as u32).collect();
    let (m0, m1) = (ms[0], ms[1]);
    let r: Ref<Option<Task<()>>> = Ref::new(None);
    let r2 = r.clone();
    let t = as_task(
        move || {
            sleep(m0);
            let Some(x) = r2.get() else {
                eprintln("no dependent");
                return;
            };
            let y = as_task(move || sleep(m1), PRIO_DEFAULT);
            wait_any(&[x, y]);
            eprintln("task's waitAny returned");
        },
        PRIO_DEFAULT,
    );
    let x = map_task(|()| eprintln("dependent ran"), t, PRIO_DEFAULT, false, true);
    r.set(Some(x.clone()));
    x.get();
    eprintln("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.map String.toNat!
//   let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
//   let t ← IO.asTask (do
//     IO.sleep ms[0]!.toUInt32
//     match ← r.get with
//     | some x =>
//       IO.eprintln "task waits for its own dependent"
//       let _ ← IO.wait x
//       IO.eprintln "task got it"
//     | none => IO.eprintln "no dependent")
//   let x ← IO.mapTask (fun _ => IO.eprintln "dependent ran") t
//   r.set (some x)
//   IO.sleep ms[1]!.toUInt32
//   IO.eprintln "main done"
fn task_waits_own_dep(args: &[String]) -> u32 {
    let ms: Vec<u32> = args.iter().map(|a| to_nat(a) as u32).collect();
    let m0 = ms[0];
    let r: Ref<Option<Task<()>>> = Ref::new(None);
    let r2 = r.clone();
    let t = as_task(
        move || {
            sleep(m0);
            match r2.get() {
                Some(x) => {
                    eprintln("task waits for its own dependent");
                    x.get();
                    eprintln("task got it");
                }
                None => eprintln("no dependent"),
            }
        },
        PRIO_DEFAULT,
    );
    let x = map_task(|()| eprintln("dependent ran"), t, PRIO_DEFAULT, false, true);
    r.set(Some(x));
    sleep(ms[1]);
    eprintln("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.map String.toNat!
//   let b ← IO.asTask (IO.sleep ms[0]!.toUInt32)
//   let a ← IO.mapTask (fun _ => IO.eprintln "async dep ran") b
//   let _ ← IO.mapTask (sync := true) (fun _ => do
//     IO.eprintln "sync dep waits for a"
//     let _ ← IO.wait a
//     IO.eprintln "sync dep got a") b
//   IO.sleep ms[1]!.toUInt32
//   IO.eprintln "main done"
fn sync_dep_waits_older(args: &[String]) -> u32 {
    let ms: Vec<u32> = args.iter().map(|a| to_nat(a) as u32).collect();
    let m0 = ms[0];
    let b = as_task(move || sleep(m0), PRIO_DEFAULT);
    let a = map_task(
        |()| eprintln("async dep ran"),
        b.clone(),
        PRIO_DEFAULT,
        false,
        true,
    );
    let _ = map_task(
        move |()| {
            eprintln("sync dep waits for a");
            a.get();
            eprintln("sync dep got a");
        },
        b,
        PRIO_DEFAULT,
        true,
        true,
    );
    sleep(ms[1]);
    eprintln("main done");
    0
}

// ---------------------------------------------------------------------------
// LB-01: a concurrent `IO.Ref.set` is not lost (`tests/cases/refs`). The
// twin gives the correct outcome, which the case expects (native loses some
// trials, a different number each run).

// def trial (bound : Nat) : IO Bool := do
//   let r ← IO.mkRef (0 : Nat)
//   let t ← IO.asTask (prio := .dedicated) (r.set 1)
//   let mut i := 0
//   while i < bound do
//     if (← r.get) == 1 then break
//     i := i + 1
//   let _ ← IO.wait t
//   return (← r.get) == 0
fn trial(bound: u64) -> bool {
    let r = Ref::new(0u64);
    let r2 = r.clone();
    let t = as_task(move || r2.set(1), PRIO_DEDICATED);
    let mut i = 0;
    while i < bound {
        if r.get() == 1 {
            break;
        }
        i += 1;
    }
    t.get();
    drop(t);
    r.get() == 0
}

// def main (args : List String) : IO Unit := do
//   let trials := args[0]!.toNat!
//   let bound := args[1]!.toNat!
//   let mut lost := 0
//   for _ in [0:trials] do
//     if ← trial bound then lost := lost + 1
//   IO.println s!"lost {lost}"
fn lost_update(args: &[String]) -> u32 {
    let trials = to_nat(&args[0]);
    let bound = to_nat(&args[1]);
    let mut lost = 0;
    for _ in 0..trials {
        if trial(bound) {
            lost += 1;
        }
    }
    println(&format!("lost {lost}"));
    0
}

// ---------------------------------------------------------------------------
// tests/cases/refs: a reference taken by `modify` while its function blocks.

// def slowValue (slow : Task (Except IO.Error Nat)) : Nat :=
//   match slow.get with
//   | .ok n => n
//   | .error _ => 0
//
// def main (args : List String) : IO Unit := do
//   let setMs := args[0]!.toNat!
//   let slowMs := args[1]!.toNat!
//   let r ← IO.mkRef (0 : Nat)
//   let slow ← IO.asTask (prio := .dedicated) do
//     IO.sleep slowMs.toUInt32
//     return 1
//   let t ← IO.asTask (prio := .dedicated) do
//     r.modify fun v => v + slowValue slow
//   IO.sleep setMs.toUInt32
//   r.set 100
//   let t0 ← IO.monoMsNow
//   let v ← r.get
//   let t1 ← IO.monoMsNow
//   let how := if t1 - t0 ≥ (slowMs - setMs) / 2 then "after modify's set" else "at once"
//   IO.println s!"get after main's set: {v}, {how}"
//   let _ ← IO.wait t
//   IO.println s!"after modify: {← r.get}"
fn set_during_modify(args: &[String]) -> u32 {
    let set_ms = to_nat(&args[0]);
    let slow_ms = to_nat(&args[1]);
    let r = Ref::new(0u64);
    let slow = as_task(
        move || {
            sleep(slow_ms as u32);
            1u64
        },
        PRIO_DEDICATED,
    );
    let r2 = r.clone();
    let t = as_task(move || r2.modify(|v| v + slow.get()), PRIO_DEDICATED);
    sleep(set_ms as u32);
    r.set(100);
    let t0 = mono_ms_now();
    let v = r.get();
    let t1 = mono_ms_now();
    let how = if t1 - t0 >= slow_ms.saturating_sub(set_ms) / 2 {
        "after modify's set"
    } else {
        "at once"
    };
    println(&format!("get after main's set: {v}, {how}"));
    t.get();
    println(&format!("after modify: {}", r.get()));
    0
}

// def main (args : List String) : IO Unit := do
//   let readMs := args[0]!.toNat!
//   let slowMs := args[1]!.toNat!
//   let r ← IO.mkRef (0 : Nat)
//   let slow ← IO.asTask (prio := .dedicated) do
//     IO.sleep slowMs.toUInt32
//     return 1
//   let t ← IO.asTask (prio := .dedicated) do
//     r.modify fun v => v + slowValue slow
//   IO.sleep readMs.toUInt32
//   let t0 ← IO.monoMsNow
//   let v ← r.get
//   let t1 ← IO.monoMsNow
//   let how := if t1 - t0 ≥ (slowMs - readMs) / 2 then "after modify's set" else "at once"
//   IO.println s!"get during modify: {v}, {how}"
//   let _ ← IO.wait t
//   IO.println s!"after modify: {← r.get}"
fn get_during_modify(args: &[String]) -> u32 {
    let read_ms = to_nat(&args[0]);
    let slow_ms = to_nat(&args[1]);
    let r = Ref::new(0u64);
    let slow = as_task(
        move || {
            sleep(slow_ms as u32);
            1u64
        },
        PRIO_DEDICATED,
    );
    let r2 = r.clone();
    let t = as_task(move || r2.modify(|v| v + slow.get()), PRIO_DEDICATED);
    sleep(read_ms as u32);
    let t0 = mono_ms_now();
    let v = r.get();
    let t1 = mono_ms_now();
    let how = if t1 - t0 >= slow_ms.saturating_sub(read_ms) / 2 {
        "after modify's set"
    } else {
        "at once"
    };
    println(&format!("get during modify: {v}, {how}"));
    t.get();
    println(&format!("after modify: {}", r.get()));
    0
}

// def main (args : List String) : IO Unit := do
//   let swapMs := args[0]!.toNat!
//   let slowMs := args[1]!.toNat!
//   let r ← IO.mkRef (0 : Nat)
//   let slow ← IO.asTask (prio := .dedicated) do
//     IO.sleep slowMs.toUInt32
//     return 1
//   let t ← IO.asTask (prio := .dedicated) do
//     r.modify fun v => v + slowValue slow
//   IO.sleep swapMs.toUInt32
//   let t0 ← IO.monoMsNow
//   let old ← r.swap 100
//   let t1 ← IO.monoMsNow
//   let how := if t1 - t0 ≥ (slowMs - swapMs) / 2 then "after modify's set" else "at once"
//   IO.println s!"swap during modify returned: {old}, {how}"
//   let _ ← IO.wait t
//   IO.println s!"after modify: {← r.get}"
fn swap_during_modify(args: &[String]) -> u32 {
    let swap_ms = to_nat(&args[0]);
    let slow_ms = to_nat(&args[1]);
    let r = Ref::new(0u64);
    let slow = as_task(
        move || {
            sleep(slow_ms as u32);
            1u64
        },
        PRIO_DEDICATED,
    );
    let r2 = r.clone();
    let t = as_task(move || r2.modify(|v| v + slow.get()), PRIO_DEDICATED);
    sleep(swap_ms as u32);
    let t0 = mono_ms_now();
    let old = r.swap(100);
    let t1 = mono_ms_now();
    let how = if t1 - t0 >= slow_ms.saturating_sub(swap_ms) / 2 {
        "after modify's set"
    } else {
        "at once"
    };
    println(&format!("swap during modify returned: {old}, {how}"));
    t.get();
    println(&format!("after modify: {}", r.get()));
    0
}

// ---------------------------------------------------------------------------
// tests/cases/taskio: blocking IO in programs with tasks (sched-io on one
// thread; in threads mode a blocking call blocks its own thread).

/// An uncaught `IO.Error` ends a Lean `main`: the twins expect none.
pub(crate) fn ok<T>(r: R<T>) -> T {
    r.unwrap_or_else(|e| panic!("uncaught IO error: {e:?}"))
}

fn tail(s: &str, n: usize) -> String {
    let len = s.chars().count();
    lio::drop_chars(s, len.saturating_sub(n))
}

// def main (args : List String) : IO Unit := do
//   let o ← IO.Process.output { cmd := "sh", args := #["-c",
//     "yes a 2>/dev/null | head -c \"$1\"; yes b 2>/dev/null | head -c \"$2\" >&2; exit 3",
//     "sh", args[0]!, args[1]!] }
//   IO.println s!"code {o.exitCode} stdout {o.stdout.length} stderr {o.stderr.length}"
//   IO.println s!"stdout {repr (o.stdout.take 4).toString} ... {repr (o.stdout.drop (o.stdout.length - 4)).toString}"
//   IO.println s!"stderr {repr (o.stderr.take 4).toString} ... {repr (o.stderr.drop (o.stderr.length - 4)).toString}"
fn output_big_stdout(args: &[String]) -> u32 {
    let script =
        "yes a 2>/dev/null | head -c \"$1\"; yes b 2>/dev/null | head -c \"$2\" >&2; exit 3";
    let o = ok(lio::output("sh", &["-c", script, "sh", &args[0], &args[1]]));
    println(&format!(
        "code {} stdout {} stderr {}",
        o.exit_code,
        o.stdout.chars().count(),
        o.stderr.chars().count()
    ));
    println(&format!(
        "stdout {} ... {}",
        quote(&lio::take(&o.stdout, 4)),
        quote(&tail(&o.stdout, 4))
    ));
    println(&format!(
        "stderr {} ... {}",
        quote(&lio::take(&o.stderr, 4)),
        quote(&tail(&o.stderr, 4))
    ));
    0
}

// def main (args : List String) : IO Unit := do
//   let o ← IO.Process.output { cmd := "sh", args := #["-c",
//     "i=0; while [ $i -lt \"$1\" ]; do yes o 2>/dev/null | head -c \"$2\"; yes e 2>/dev/null | head -c \"$2\" >&2; i=$((i+1)); done",
//     "sh", args[0]!, args[1]!] }
//   IO.println s!"code {o.exitCode} stdout {o.stdout.length} stderr {o.stderr.length}"
//   IO.println s!"stdout all o: {o.stdout.all (fun c => c == 'o' || c == '\n')} stderr all e: {o.stderr.all (fun c => c == 'e' || c == '\n')}"
fn output_both_overflow(args: &[String]) -> u32 {
    let script = "i=0; while [ $i -lt \"$1\" ]; do yes o 2>/dev/null | head -c \"$2\"; yes e 2>/dev/null | head -c \"$2\" >&2; i=$((i+1)); done";
    let o = ok(lio::output("sh", &["-c", script, "sh", &args[0], &args[1]]));
    println(&format!(
        "code {} stdout {} stderr {}",
        o.exit_code,
        o.stdout.chars().count(),
        o.stderr.chars().count()
    ));
    println(&format!(
        "stdout all o: {} stderr all e: {}",
        o.stdout.chars().all(|c| c == 'o' || c == '\n'),
        o.stderr.chars().all(|c| c == 'e' || c == '\n')
    ));
    0
}

// def writeAll (stdin : IO.FS.Handle) (n len : Nat) : IO Unit := do
//   let line := String.ofList (List.replicate len 'x') ++ "\n"
//   for _ in [0:n] do
//     stdin.putStr line
//   stdin.flush
fn write_all(stdin: Handle, n: u64, len: u64) -> R<()> {
    let mut line = "x".repeat(len as usize);
    line.push('\n');
    for _ in 0..n {
        stdin.put_str(line.as_bytes())?;
    }
    stdin.flush()
    // `stdin`, owned, is dropped here: the pipe closes
}

// def main (args : List String) : IO Unit := do
//   let n := args[0]!.toNat!
//   let len := args[1]!.toNat!
//   let child ← IO.Process.spawn { cmd := "cat", stdin := .piped, stdout := .piped }
//   let (stdin, child) ← child.takeStdin
//   let reader ← IO.asTask do
//     let s ← child.stdout.readToEnd
//     return (s.length, (s.splitOn "\n").length - 1)
//   writeAll stdin n len
//   IO.println "main: wrote everything"
//   let r ← IO.wait reader
//   match r with
//   | .ok (bytes, lines) => IO.println s!"task: read {bytes} bytes in {lines} lines"
//   | .error e => IO.println s!"task failed: {e}"
//   let code ← child.wait
//   IO.println s!"cat exited {code}"
fn task_reads_main_writes(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let len = to_nat(&args[1]);
    let child = ok(lio::spawn(
        "cat",
        &[],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    ));
    let stdin = child.stdin.expect("piped");
    let out = child.stdout.expect("piped");
    let reader = as_task(
        move || lio::read_to_end(&out).map(|s| (s.chars().count(), s.split('\n').count() - 1)),
        PRIO_DEFAULT,
    );
    ok(write_all(stdin, n, len));
    println("main: wrote everything");
    match reader.get() {
        Ok((bytes, lines)) => println(&format!("task: read {bytes} bytes in {lines} lines")),
        Err(e) => println(&format!("task failed: {e:?}")),
    }
    let code = ok(child.process.wait());
    println(&format!("cat exited {code}"));
    0
}

// def main (args : List String) : IO Unit := do
//   let ticks := args[1]!.toNat!
//   let child ← IO.Process.spawn { cmd := "sh", args := #["-c", "sleep \"$1\"; exit 7", "sh", args[0]!] }
//   let waiter ← IO.asTask (prio := .dedicated) do
//     let c ← child.wait
//     IO.println s!"waiter: child exited {c}"
//     return c
//   let ticker ← IO.asTask do
//     for i in [0:ticks] do
//       IO.sleep 50
//       IO.println s!"ticker: {i}"
//   let _ ← IO.wait ticker
//   IO.println "main: ticker done"
//   let r ← IO.wait waiter
//   IO.println s!"main: waiter returned {repr r.toOption}"
fn wait_in_task(args: &[String]) -> u32 {
    let ticks = to_nat(&args[1]);
    let child = ok(lio::spawn(
        "sh",
        &["-c", "sleep \"$1\"; exit 7", "sh", &args[0]],
        lio::INHERIT,
    ));
    let p = child.process.clone();
    let waiter = as_task(
        move || {
            let c = p.wait()?;
            println(&format!("waiter: child exited {c}"));
            Ok(c)
        },
        PRIO_DEDICATED,
    );
    let ticker = as_task(
        move || {
            for i in 0..ticks {
                sleep(50);
                println(&format!("ticker: {i}"));
            }
        },
        PRIO_DEFAULT,
    );
    ticker.get();
    println("main: ticker done");
    let r: R<u32> = waiter.get();
    println(&format!(
        "main: waiter returned {}",
        match r {
            Ok(c) => format!("some {c}"),
            Err(_) => "none".into(),
        }
    ));
    0
}

// def main (args : List String) : IO Unit := do
//   let ticks := args[1]!.toNat!
//   let ticker ← IO.asTask do
//     for i in [0:ticks] do
//       IO.sleep 50
//       IO.println s!"ticker: {i}"
//   let o ← IO.Process.output { cmd := "sh", args := #["-c", "sleep \"$1\"; echo out; echo err >&2; exit 2", "sh", args[0]!] }
//   IO.println s!"output: {o.exitCode} {repr o.stdout} {repr o.stderr}"
//   let _ ← IO.wait ticker
//   IO.println "done"
//
// `IO.Process.output` through the runtime's override (`process::output`),
// not Lean's definition: its wait for both pipes and the child lets the
// ticker run.
fn output_while_ticking(args: &[String]) -> u32 {
    let ticks = to_nat(&args[1]);
    let ticker = as_task(
        move || {
            for i in 0..ticks {
                sleep(50);
                println(&format!("ticker: {i}"));
            }
        },
        PRIO_DEFAULT,
    );
    let script = "sleep \"$1\"; echo out; echo err >&2; exit 2";
    let a: [&[u8]; 4] = [b"-c", script.as_bytes(), b"sh", args[0].as_bytes()];
    let spawn_args = lean_runtime::io::process::SpawnArgs {
        cmd: b"sh",
        args: &a,
        cwd: None,
        env: &[],
        inherit_env: true,
        setsid: false,
    };
    // a process spawn is an effect point
    lean_runtime::sched::effect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = ok(lean_runtime::io::process::output(
        &spawn_args,
        None,
        &mut out,
        &mut err,
    ));
    println(&format!(
        "output: {code} {} {}",
        quote(&String::from_utf8_lossy(&out)),
        quote(&String::from_utf8_lossy(&err))
    ));
    ticker.get();
    println("done");
    0
}

// ---------------------------------------------------------------------------
// sched-3: what a waiter or a poller may run on its own stack (reviews
// AR-9, AR-10). Each case runs with `LEAN_NUM_THREADS=1`.

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let x ← IO.asTask (do IO.sleep ms.toUInt32; IO.println "X")
//   IO.sleep 20
//   let b ← IO.asTask (IO.println "B")
//   let c ← IO.asTask (IO.println "C")
//   let a ← IO.asTask (prio := .max) (do let _ ← IO.wait c; IO.println "A")
//   let _ ← IO.wait a
//   let _ ← IO.wait b
//   let _ ← IO.wait x
//   IO.println "main done"
fn wait_queue_order(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let x = as_task(
        move || {
            sleep(ms);
            println("X");
        },
        PRIO_DEFAULT,
    );
    sleep(20);
    let b = as_task(|| println("B"), PRIO_DEFAULT);
    let c = as_task(|| println("C"), PRIO_DEFAULT);
    let a = as_task(
        move || {
            c.get();
            println("A");
        },
        PRIO_MAX,
    );
    a.get();
    b.get();
    x.get();
    println("main done");
    0
}

// def main (_args : List String) : IO Unit := do
//   let p : IO.Promise Nat ← IO.Promise.new
//   let b ← IO.asTask (do
//     let v ← IO.wait p.result?
//     IO.println s!"b got {v.getD 0}")
//   let c ← IO.asTask (IO.println "c ran")
//   let _ ← IO.wait c
//   IO.println "main resolves"
//   p.resolve 7
//   let _ ← IO.wait b
//   IO.println "main done"
fn wait_head_blocks_on_main(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let r = p.result_opt();
    let b = as_task(
        move || {
            let v = r.get();
            println(&format!("b got {}", v.unwrap_or(0)));
        },
        PRIO_DEFAULT,
    );
    let c = as_task(|| println("c ran"), PRIO_DEFAULT);
    c.get();
    println("main resolves");
    p.resolve(7);
    b.get();
    println("main done");
    0
}

// def main (_args : List String) : IO Unit := do
//   let p : IO.Promise Nat ← IO.Promise.new
//   let a ← IO.asTask (do
//     let v ← IO.wait p.result?
//     IO.println s!"a got {v.getD 0}"
//     pure 1)
//   let b ← IO.asTask (do IO.println "b ran"; pure 2)
//   let r ← IO.waitAny [a, b]
//   IO.println s!"waitAny: {r}"
//   p.resolve 7
//   let _ ← IO.wait a
//   IO.println "main done"
fn wait_any_head_blocks_on_main(_: &[String]) -> u32 {
    let p: Promise<u64> = Promise::new();
    let pr = p.result_opt();
    let a = as_task(
        move || {
            let v = pr.get();
            println(&format!("a got {}", v.unwrap_or(0)));
            1u64
        },
        PRIO_DEFAULT,
    );
    let b = as_task(
        || {
            println("b ran");
            2u64
        },
        PRIO_DEFAULT,
    );
    let r = wait_any(&[a.clone(), b]);
    println(&format!("waitAny: ok: {r}"));
    p.resolve(7);
    a.get();
    println("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let ms := args.head!.toNat!
//   let p : IO.Promise Nat ← IO.Promise.new
//   let t ← IO.asTask (do
//     let _ ← IO.waitAny [p.result?]
//     IO.println "T done")
//   IO.sleep 20
//   let b ← IO.asTask (IO.println "B")
//   IO.sleep ms.toUInt32
//   IO.println "main resolves"
//   p.resolve 1
//   let _ ← IO.wait t
//   let _ ← IO.wait b
//   IO.println "main done"
fn wait_any_keeps_worker(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let p: Promise<u64> = Promise::new();
    let pr = p.result_opt();
    let t = as_task(
        move || {
            let _ = wait_any(&[pr]);
            println("T done");
        },
        PRIO_DEFAULT,
    );
    sleep(20);
    let b = as_task(|| println("B"), PRIO_DEFAULT);
    sleep(ms);
    println("main resolves");
    p.resolve(1);
    t.get();
    b.get();
    println("main done");
    0
}

// def main (_args : List String) : IO Unit := do
//   let b ← IO.asTask (IO.println "B")
//   let t ← IO.asTask (IO.println "T")
//   while !(← IO.hasFinished t) do
//     pure ()
//   let _ ← IO.wait b
//   IO.println "main done"
fn poll_queue_order(_: &[String]) -> u32 {
    let b = as_task(|| println("B"), PRIO_DEFAULT);
    let t = as_task(|| println("T"), PRIO_DEFAULT);
    while !has_finished(&t) {}
    b.get();
    println("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let p : IO.Promise Nat ← IO.Promise.new
//   let t ← IO.asTask (do
//     let v ← IO.wait p.result?
//     IO.println s!"t got {v.getD 0}")
//   let mut k := 0
//   for _ in [0:n] do
//     if ← IO.hasFinished t then k := k + 1
//   IO.println s!"finished while polling: {k}"
//   p.resolve 7
//   let _ ← IO.wait t
//   IO.println "main done"
fn poll_threshold_promise(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let p: Promise<u64> = Promise::new();
    let pr = p.result_opt();
    let t = as_task(
        move || {
            let v = pr.get();
            println(&format!("t got {}", v.unwrap_or(0)));
        },
        PRIO_DEFAULT,
    );
    let mut k = 0;
    for _ in 0..n {
        if has_finished(&t) {
            k += 1;
        }
    }
    println(&format!("finished while polling: {k}"));
    p.resolve(7);
    t.get();
    println("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let m ← BaseMutex.new
//   m.lock
//   let t ← IO.asTask (do
//     m.lock
//     IO.println "t has the lock"
//     m.unlock)
//   let mut k := 0
//   for _ in [0:n] do
//     if ← IO.hasFinished t then k := k + 1
//   IO.println s!"finished while polling: {k}"
//   m.unlock
//   let _ ← IO.wait t
//   IO.println "main done"
fn poll_threshold_mutex(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let m = Obj::new(Mutex::new());
    m.lock();
    let m2 = m.clone();
    let t = as_task(
        move || {
            m2.lock();
            println("t has the lock");
            m2.unlock();
        },
        PRIO_DEFAULT,
    );
    let mut k = 0;
    for _ in 0..n {
        if has_finished(&t) {
            k += 1;
        }
    }
    println(&format!("finished while polling: {k}"));
    m.unlock();
    t.get();
    println("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let stop ← IO.mkRef false
//   let ticker ← IO.asTask (prio := .dedicated) (do
//     while !(← stop.get) do IO.sleep 20)
//   let a ← IO.asTask (IO.println "a ran")
//   let t := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   IO.println s!"t = {t.get}"
//   stop.set true
//   let _ ← IO.wait ticker
//   let _ ← IO.wait a
//   IO.println "main done"
fn wait_picked_pure(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let stop = Ref::new(false);
    let s2 = stop.clone();
    let ticker = as_task(
        move || {
            while !s2.get() {
                sleep(20);
            }
        },
        PRIO_DEDICATED,
    );
    let a = as_task(|| println("a ran"), PRIO_DEFAULT);
    let t = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    println(&format!("t = {}", t.get()));
    stop.set(true);
    ticker.get();
    a.get();
    println("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let stop ← IO.mkRef false
//   let ticker ← IO.asTask (prio := .dedicated) (do
//     while !(← stop.get) do IO.sleep 20)
//   let t1 := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   let t2 := Task.spawn fun _ => (List.range (n + 1)).foldl (· + ·) 0
//   let r ← IO.waitAny [t1, t2]
//   IO.println s!"waitAny: {r}"
//   stop.set true
//   let _ ← IO.wait ticker
//   IO.println "main done"
fn wait_any_picked_pure(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let stop = Ref::new(false);
    let s2 = stop.clone();
    let ticker = as_task(
        move || {
            while !s2.get() {
                sleep(20);
            }
        },
        PRIO_DEDICATED,
    );
    let t1 = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    let t2 = Task::spawn(move || (0..n + 1).sum::<u64>(), PRIO_DEFAULT);
    let r = wait_any(&[t1, t2]);
    println(&format!("waitAny: {r}"));
    stop.set(true);
    ticker.get();
    println("main done");
    0
}

// def main (_args : List String) : IO Unit := do
//   let p : IO.Promise Nat ← IO.Promise.new
//   let t2 ← IO.asTask (pure (some 2))
//   let _d ← IO.mapTask (sync := true) (fun _ => do
//     let _ ← IO.asTask (pure 0)
//     IO.sleep 200) t2
//   let u ← IO.asTask (do
//     let v ← IO.wait p.result?
//     IO.println s!"u got {v}"
//     pure v)
//   match ← IO.waitAny [t2, u] with
//   | .ok v => IO.println s!"waitAny returned {v}"
//   | .error e => IO.println s!"waitAny error {e}"
//   p.resolve 7
//   let _ ← IO.wait u
//   IO.println "done"
fn wait_any_finished_unnotified(_: &[String]) -> u32 {
    fn opt(v: Option<u64>) -> String {
        match v {
            Some(n) => format!("(some {n})"),
            None => "none".into(),
        }
    }
    let p: Promise<u64> = Promise::new();
    let t2 = as_task(|| Some(2u64), PRIO_DEFAULT);
    // `_d` is unused: compiled Lean drops it at once.
    drop(map_task(
        |_: Option<u64>| {
            drop(as_task(|| 0u64, PRIO_DEFAULT));
            sleep(200);
        },
        t2.clone(),
        PRIO_DEFAULT,
        true,
        true,
    ));
    let pr = p.result_opt();
    let u = as_task(
        move || {
            let v = pr.get();
            println(&format!("u got {}", opt(v)));
            v
        },
        PRIO_DEFAULT,
    );
    let r = wait_any(&[t2, u.clone()]);
    println(&format!("waitAny returned {}", opt(r)));
    p.resolve(7);
    u.get();
    println("done");
    0
}

// ---------------------------------------------------------------------------
// sched-4 (AR-15). Each case runs with `LEAN_NUM_THREADS=1`.

// def main (_args : List String) : IO Unit := do
//   let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
//   let t ← IO.asTask (do
//     IO.sleep 50
//     match ← r.get with
//     | some d =>
//       IO.eprintln "t waits for its own dependent"
//       let _ ← IO.wait d
//     | none => IO.eprintln "no dependent")
//   let d ← IO.mapTask (fun _ => pure ()) t
//   r.set (some d)
//   IO.sleep 20
//   let _b ← IO.asTask (IO.eprintln "B ran")
//   IO.sleep 300
//   IO.eprintln "main done"
fn self_wait_frees_worker(_: &[String]) -> u32 {
    let r: Ref<Option<Task<()>>> = Ref::new(None);
    let r2 = r.clone();
    let t = as_task(
        move || {
            sleep(50);
            match r2.get() {
                Some(d) => {
                    eprintln("t waits for its own dependent");
                    d.get();
                }
                None => eprintln("no dependent"),
            }
        },
        PRIO_DEFAULT,
    );
    let d = map_task(|_: ()| (), t, PRIO_DEFAULT, false, true);
    r.set(Some(d));
    sleep(20);
    // `_b` is unused: compiled Lean drops it at once.
    drop(as_task(|| eprintln("B ran"), PRIO_DEFAULT));
    sleep(300);
    eprintln("main done");
    0
}

// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let ms := args[1]!.toNat!
//   let t := Task.spawn fun _ => spin s 0
//   let f1 ← IO.hasFinished t
//   let _io ← IO.asTask (IO.eprintln "io task ran")
//   IO.sleep ms.toUInt32
//   let f2 ← IO.hasFinished t
//   IO.eprintln s!"main done {f1} {f2}"
fn runaway_pure_task_before_io(args: &[String]) -> u32 {
    let s = seed(args);
    let ms = to_nat(&args[1]) as u32;
    let t = Task::spawn(move || spin(s, 0), PRIO_DEFAULT);
    let f1 = has_finished(&t);
    // `_io` is unused: compiled Lean drops it at once.
    drop(as_task(|| eprintln("io task ran"), PRIO_DEFAULT));
    sleep(ms);
    let f2 = has_finished(&t);
    drop(t);
    eprintln(&format!("main done {f1} {f2}"));
    0
}

// def main (_args : List String) : IO Unit := do
//   let s ← IO.asTask (IO.sleep 50)
//   let _d ← IO.mapTask (sync := true) (fun _ => do
//     IO.sleep 100
//     IO.eprintln "D done") s
//   IO.sleep 20
//   let _b ← IO.asTask (IO.eprintln "B ran")
//   IO.sleep 300
//   IO.eprintln "main done"
fn sync_walk_keeps_worker(_: &[String]) -> u32 {
    let s = as_task(|| sleep(50), PRIO_DEFAULT);
    // `_d` is unused: compiled Lean drops it at once (an IO task still runs).
    drop(map_task(
        |_: ()| {
            sleep(100);
            eprintln("D done");
        },
        s,
        PRIO_DEFAULT,
        true,
        true,
    ));
    sleep(20);
    drop(as_task(|| eprintln("B ran"), PRIO_DEFAULT));
    sleep(300);
    eprintln("main done");
    0
}

// def main (_args : List String) : IO Unit := do
//   let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
//   let s ← IO.asTask (IO.sleep 50)
//   let d ← IO.mapTask (sync := true) (fun _ => do
//     match ← r.get with
//     | some e =>
//       IO.eprintln "d waits for its own dependent"
//       let _ ← IO.wait e
//     | none => IO.eprintln "no dependent") s
//   let e ← IO.mapTask (fun _ => pure ()) d
//   r.set (some e)
//   IO.sleep 20
//   let _b ← IO.asTask (IO.eprintln "B ran")
//   IO.sleep 300
//   IO.eprintln "main done"
fn sync_self_wait_keeps_worker(_: &[String]) -> u32 {
    let r: Ref<Option<Task<()>>> = Ref::new(None);
    let s = as_task(|| sleep(50), PRIO_DEFAULT);
    let r2 = r.clone();
    let d = map_task(
        move |_: ()| match r2.get() {
            Some(e) => {
                eprintln("d waits for its own dependent");
                e.get();
            }
            None => eprintln("no dependent"),
        },
        s,
        PRIO_DEFAULT,
        true,
        true,
    );
    let e = map_task(|_: ()| (), d, PRIO_DEFAULT, false, true);
    r.set(Some(e));
    sleep(20);
    drop(as_task(|| eprintln("B ran"), PRIO_DEFAULT));
    sleep(300);
    eprintln("main done");
    0
}

// tests/cases/tasks/sync_wait_in_inline_walk.lean (our review's probe
// SyncSelfWait, RS4-01; its twin rv4_sync_self_wait)
fn sync_wait_in_inline_walk(_: &[String]) -> u32 {
    let r: Ref<Option<Task<()>>> = Ref::new(None);
    let r2 = r.clone();
    // `_p` is unused: compiled Lean drops it at once (an IO task still runs).
    drop(as_task(
        move || {
            sleep(50);
            if let Some(x) = r2.get() {
                x.get();
            }
        },
        PRIO_DEFAULT,
    ));
    let x = as_task(|| eprintln("x ran"), PRIO_DEFAULT);
    let d = map_task(
        |_: ()| eprintln("d ran"),
        x.clone(),
        PRIO_DEFAULT,
        false,
        true,
    );
    drop(map_task(
        move |_: ()| {
            d.get();
            eprintln("s done");
        },
        x.clone(),
        PRIO_DEFAULT,
        true,
        true,
    ));
    r.set(Some(x));
    sleep(20);
    drop(as_task(|| eprintln("B ran"), PRIO_DEFAULT));
    sleep(300);
    eprintln("main done");
    0
}

// def main (_args : List String) : IO Unit := do
//   let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
//   let s ← IO.asTask (IO.sleep 50)
//   let _d ← IO.mapTask (sync := true) (fun _ => do
//     match ← r.get with
//     | some q =>
//       IO.eprintln "d waits for q"
//       let _ ← IO.wait q
//       IO.eprintln "d done"
//     | none => IO.eprintln "no q") s
//   IO.sleep 20
//   let q ← IO.asTask (IO.eprintln "q ran")
//   r.set (some q)
//   IO.sleep 300
//   IO.eprintln "main done"
fn sync_dep_waits_queued_task(_: &[String]) -> u32 {
    let r: Ref<Option<Task<()>>> = Ref::new(None);
    let s = as_task(|| sleep(50), PRIO_DEFAULT);
    let r2 = r.clone();
    drop(map_task(
        move |_: ()| match r2.get() {
            Some(q) => {
                eprintln("d waits for q");
                q.get();
                eprintln("d done");
            }
            None => eprintln("no q"),
        },
        s,
        PRIO_DEFAULT,
        true,
        true,
    ));
    sleep(20);
    let q = as_task(|| eprintln("q ran"), PRIO_DEFAULT);
    r.set(Some(q));
    sleep(300);
    eprintln("main done");
    0
}

// ---------------------------------------------------------------------------
// tests/cases/tasks/worker_keeps_{streams,errno}.lean (review RT2-L-01,
// AR-24): a pool worker keeps its thread's streams and `errno` from one task
// to the next; the scheduler swaps each context's and each emulated
// worker's (`lean_runtime::sched`'s `slots`).

/// The current stdout as these twins set it: a buffer
/// (`IO.FS.Stream.ofBuffer`), or `None` for the process's stream.
type OutStream = Option<Obj<Var<Vec<u8>>>>;

/// `IO.println` on the current stdout.
fn out_println(s: &str) {
    use lean_runtime::io::streams::{current, StdStream};
    let cur: OutStream = current(StdStream::Stdout, || None);
    match cur {
        Some(b) => {
            lean_runtime::sched::effect();
            b.borrow_mut()
                .extend_from_slice(format!("{s}\n").as_bytes());
        }
        None => println(s),
    }
}

fn worker_keeps_streams(_: &[String]) -> u32 {
    let buf: Obj<Var<Vec<u8>>> = Obj::default();
    let b2 = buf.clone();
    let a = as_task(
        move || {
            let _ = lean_runtime::io::streams::set_stdout(Some(b2) as OutStream, || None);
            out_println("A: before its sleep");
            sleep(200);
            out_println("A: after its sleep");
        },
        PRIO_DEFAULT,
    );
    sleep(100);
    out_println("main: while A sleeps");
    a.get();
    let b = as_task(|| out_println("B: after A, on A's worker"), PRIO_DEFAULT);
    b.get();
    let c = as_task(|| out_println("C: a thread of its own"), PRIO_DEDICATED);
    c.get();
    let text = String::from_utf8(buf.borrow().clone()).expect("UTF-8");
    out_println(&format!("A's buffer: {}", quote(&text)));
    0
}

fn worker_keeps_errno(_: &[String]) -> u32 {
    use lean_runtime::io::fs;
    ok(lio::write_file("e.txt", "line1\n"));
    let h = ok(Handle::open(b"e.txt", FsMode::Read));
    let _ = h.put_str(b"x").and_then(|()| h.flush());
    let a = as_task(
        || {
            let _ = fs::remove_dir(b"e.txt");
        },
        PRIO_DEFAULT,
    );
    a.get();
    let _ = fs::create_dir(b"e.txt");
    let h2 = h.clone();
    let b = as_task(
        move || match lio::get_line(&h2) {
            Ok(l) => format!("ok {}", quote(&l)),
            Err(e) => lio::error_text(&e),
        },
        PRIO_DEFAULT,
    );
    println(&format!("B's getLine: {}", b.get()));
    match lio::get_line(&h) {
        Ok(l) => println(&format!("main's getLine: ok {}", quote(&l))),
        Err(e) => println(&format!("main's getLine: {}", lio::error_text(&e))),
    }
    0
}

// ---------------------------------------------------------------------------
// fixes-3: a started pure task keeps its worker until it has run (review
// AR-25). Each case runs with `LEAN_NUM_THREADS=1`.

/// `dbgTrace msg`: Lean's `io_eprintln` on the current standard error, an
/// output, so an effect point first, as the glue's other outputs.
fn dbg_trace(msg: &str) {
    lean_runtime::sched::effect();
    lean_runtime::io::debug::dbg_trace(msg.as_bytes());
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   IO.eprintln "start"
//   let ts := (List.range n).map fun i => Task.spawn fun _ => dbgTrace s!"task {i}" fun _ => i
//   IO.eprintln s!"values {ts.reverse.map Task.get}"
fn wait_pure_queue_order(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    eprintln("start");
    let ts: Vec<Task<u64>> = (0..n)
        .map(|i| {
            Task::spawn(
                move || {
                    dbg_trace(&format!("task {i}"));
                    i
                },
                PRIO_DEFAULT,
            )
        })
        .collect();
    let vs: Vec<String> = ts.iter().rev().map(|t| t.get().to_string()).collect();
    eprintln(&format!("values [{}]", vs.join(", ")));
    0
}

// def slow (n : Nat) : Nat := Id.run do
//   let mut s := 0
//   for i in [0:n] do s := (s + i * i) % 1000003
//   return s
fn slow(n: u64) -> u64 {
    let mut s = 0u64;
    for i in 0..n {
        s = (s + i * i) % 1000003;
    }
    s
}

// partial def spinForever (n : Nat) : Nat := if n == 0 then 1 else spinForever (n + 1)
fn spin_forever(mut n: u64) -> u64 {
    // `Nat`: no overflow; the loop never ends for n > 0
    while n != 0 {
        n = n.wrapping_add(1).max(1);
        std::hint::black_box(n);
    }
    1
}

// @[noinline] def mkT1 (k : Nat) : Task Nat := Task.spawn fun _ => spinForever (k + 1)
//
// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let busy ← IO.asTask (do IO.sleep 50; return 1)
//   let t0 := Task.spawn fun _ => slow k
//   let f0 ← IO.hasFinished t0
//   let keep ← IO.mkRef (some (mkT1 k))
//   let _ ← IO.wait busy
//   keep.set none
//   IO.println s!"t0 {t0.get} {f0}"
fn drop_queued_behind_pure(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    let busy = as_task(
        || {
            sleep(50);
            1u64
        },
        PRIO_DEFAULT,
    );
    let t0 = Task::spawn(move || slow(k), PRIO_DEFAULT);
    let f0 = has_finished(&t0);
    let keep = Ref::new(Some(Task::spawn(move || spin_forever(k + 1), PRIO_DEFAULT)));
    busy.get();
    drop(busy);
    keep.set(None);
    println(&format!("t0 {} {f0}", t0.get()));
    0
}

// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let n := args[1]!.toNat!
//   let p := Task.spawn fun _ => spin s 0
//   let _ ← IO.hasFinished p
//   let q := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   let _ ← IO.hasFinished q
//   let t := Task.spawn fun _ => n + 1
//   IO.eprintln s!"t = {t.get}"
//   IO.eprintln s!"p finished: {← IO.hasFinished p}, q finished: {← IO.hasFinished q}"
fn runaway_pure_before_awaited(args: &[String]) -> u32 {
    let s = seed(args);
    let n = to_nat(&args[1]);
    let p = Task::spawn(move || spin(s, 0), PRIO_DEFAULT);
    let _ = has_finished(&p);
    let q = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    let _ = has_finished(&q);
    let t = Task::spawn(move || n + 1, PRIO_DEFAULT);
    eprintln(&format!("t = {}", t.get()));
    drop(t);
    let (fp, fq) = (has_finished(&p), has_finished(&q));
    eprintln(&format!("p finished: {fp}, q finished: {fq}"));
    0
}

// ---------------------------------------------------------------------------
// The review of fixes-3 (RF3): a started pure task and the workers.

/// `IO.eprintln s` through the current stderr (`IO.getStderr`): the buffer
/// `IO.setStderr` set (`set_stderr_stream`), else the process's stderr.
fn eprintln_current(s: &str) {
    use lean_runtime::io::streams::{current, StdStream};
    match current(StdStream::Stderr, || None as ErrStream) {
        Some(b) => {
            lean_runtime::sched::effect();
            b.borrow_mut()
                .extend_from_slice(format!("{s}\n").as_bytes());
        }
        None => eprintln(s),
    }
}

// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let buf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
//   let p := Task.spawn fun _ => dbgTrace "p's trace" fun _ => slow k
//   let _ ← IO.hasFinished p
//   IO.sleep 20
//   let x ← IO.asTask (do
//     let _ ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
//     IO.eprintln "x")
//   let _ ← IO.wait x
//   IO.println s!"p = {p.get}"
//   IO.println s!"x's buffer: {repr (String.fromUTF8! (← buf.get).data)}"
fn picked_task_own_worker_streams(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    let buf: Obj<Var<Vec<u8>>> = Obj::default();
    let p = Task::spawn(
        move || {
            dbg_trace("p's trace");
            slow(k)
        },
        PRIO_DEFAULT,
    );
    let _ = has_finished(&p);
    sleep(20);
    let b2 = buf.clone();
    let x = as_task(
        move || {
            let _ = set_stderr_stream(Some(b2));
            eprintln_current("x");
        },
        PRIO_DEFAULT,
    );
    x.get();
    drop(x);
    println(&format!("p = {}", p.get()));
    let text = String::from_utf8(buf.borrow().clone()).expect("UTF-8");
    println(&format!("x's buffer: {}", quote(&text)));
    0
}

// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let a ← IO.asTask (do IO.sleep 200; return 1)
//   IO.sleep 20
//   let p := Task.spawn fun _ => slow k
//   let _ ← IO.hasFinished p
//   IO.sleep 20
//   let t := Task.spawn fun _ => k + 1
//   let v := t.get
//   IO.eprintln s!"t = {v}, p finished then: {← IO.hasFinished p}"
//   let _ ← IO.wait a
//   IO.eprintln s!"p = {p.get}"
fn picked_task_sleeping_worker(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    let a = as_task(
        || {
            sleep(200);
            1u64
        },
        PRIO_DEFAULT,
    );
    sleep(20);
    let p = Task::spawn(move || slow(k), PRIO_DEFAULT);
    let _ = has_finished(&p);
    sleep(20);
    let t = Task::spawn(move || k + 1, PRIO_DEFAULT);
    let v = t.get();
    drop(t);
    let fp = has_finished(&p);
    eprintln(&format!("t = {v}, p finished then: {fp}"));
    a.get();
    eprintln(&format!("p = {}", p.get()));
    0
}

// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let _ticker ← IO.asTask (do while !(← IO.checkCanceled) do IO.sleep 100)
//   IO.sleep 50
//   let p := Task.spawn fun _ => slow k
//   let _ ← IO.hasFinished p
//   let t := Task.spawn fun _ => k % 7
//   IO.eprintln s!"t = {t.get}"
fn picked_task_ticking_worker(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    // `_ticker` is unused: compiled Lean drops it at once (an IO task still
    // runs)
    drop(as_task(
        || {
            while !check_canceled() {
                sleep(100);
            }
        },
        PRIO_DEFAULT,
    ));
    sleep(50);
    let p = Task::spawn(move || slow(k), PRIO_DEFAULT);
    let _ = has_finished(&p);
    let t = Task::spawn(move || k % 7, PRIO_DEFAULT);
    eprintln(&format!("t = {}", t.get()));
    0
}

// partial def loopST {σ : Type} (r : ST.Ref σ UInt64) (acc : UInt64) : ST σ UInt64 := do
//   let x ← r.get
//   if x == 0 then return acc
//   r.set (x * 6364136223846793005 + 1442695040888963407)
//   loopST r (acc + 1)
//
// def spinST (s : UInt64) : UInt64 := runST fun _ => do
//   let r ← ST.mkRef s
//   loopST r 0
fn spin_st(s: u64) -> u64 {
    let r = Ref::new(s);
    let mut acc = 0u64;
    loop {
        let x = r.get();
        if x == 0 {
            return acc;
        }
        r.set(
            x.wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        );
        acc = acc.wrapping_add(1);
    }
}

// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let n := args[1]!.toNat!
//   let p := Task.spawn fun _ => spinST s
//   let _ ← IO.hasFinished p
//   let q := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   let _ ← IO.hasFinished q
//   let t := Task.spawn fun _ => n + 1
//   IO.eprintln s!"t = {t.get}"
//   IO.eprintln s!"p finished: {← IO.hasFinished p}, q finished: {← IO.hasFinished q}"
fn picked_task_reaches_yield_points(args: &[String]) -> u32 {
    let s = seed(args);
    let n = to_nat(&args[1]);
    let p = Task::spawn(move || spin_st(s), PRIO_DEFAULT);
    let _ = has_finished(&p);
    let q = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    let _ = has_finished(&q);
    let t = Task::spawn(move || n + 1, PRIO_DEFAULT);
    eprintln(&format!("t = {}", t.get()));
    drop(t);
    let (fp, fq) = (has_finished(&p), has_finished(&q));
    eprintln(&format!("p finished: {fp}, q finished: {fq}"));
    0
}

// @[noinline] def mkP1 (k : Nat) : Task Nat := Task.spawn fun _ => spinForever (k + 1)
//
// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let p0 := Task.spawn fun _ => slow k
//   let _ ← IO.hasFinished p0
//   let keep ← IO.mkRef (some (mkP1 k))
//   let io ← IO.asTask (IO.eprintln "io")
//   IO.sleep 300
//   keep.set none
//   let _ ← IO.wait io
//   IO.eprintln s!"done {p0.get}"
fn runaway_pure_passed_over(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    let p0 = Task::spawn(move || slow(k), PRIO_DEFAULT);
    let _ = has_finished(&p0);
    let keep = Ref::new(Some(Task::spawn(move || spin_forever(k + 1), PRIO_DEFAULT)));
    let io = as_task(|| eprintln("io"), PRIO_DEFAULT);
    sleep(300);
    keep.set(None);
    io.get();
    drop(io);
    eprintln(&format!("done {}", p0.get()));
    0
}

// ---------------------------------------------------------------------------
// Review AR-33 (fixes-3): the workers' streams at the exit.

/// A stdout the twins set to a handle (`IO.FS.Stream.ofHandle h`), or
/// `None` for the process's.
type HandleOut = Option<lean_runtime::io::Handle>;

/// `IO.print s`: an effect point, then `putStr` on the current stdout.
fn print_current(s: &str) {
    use lean_runtime::io::streams::{current, StdStream};
    lean_runtime::sched::effect();
    match current(StdStream::Stdout, || None as HandleOut) {
        Some(h) => {
            let _ = h.put_str(s.as_bytes());
        }
        None => {
            let _ = lean_runtime::io::Handle::stdout().put_str(s.as_bytes());
        }
    }
}

// def main (args : List String) : IO Unit := do
//   let n := args.length
//   let t ← IO.asTask (do
//     let h ← IO.FS.Handle.mk "/dev/stdout" .write
//     discard <| IO.setStdout (IO.FS.Stream.ofHandle h)
//     IO.print s!"A{n}")
//   let _ ← IO.wait t
//   IO.print "B"
fn worker_streams_closed_at_exit(args: &[String]) -> u32 {
    let n = args.len();
    let t = as_task(
        move || {
            let h = lean_runtime::io::Handle::open(b"/dev/stdout", lean_runtime::io::FsMode::Write)
                .expect("/dev/stdout");
            let _ = lean_runtime::io::streams::set_stdout(Some(h) as HandleOut, || None);
            print_current(&format!("A{n}"));
        },
        PRIO_DEFAULT,
    );
    t.get();
    drop(t);
    print_current("B");
    0
}

// ... the same, then
//   IO.Process.exit 0
fn worker_streams_at_process_exit(args: &[String]) -> u32 {
    worker_streams_closed_at_exit(args);
    crate::glue::process_exit(0)
}

// ---------------------------------------------------------------------------
// Review AR-34 (fixes-4): the workers' streams end before the dedicated
// tasks are waited for.

/// `IO.println s` on the current stdout (`print_current`, with `\n`).
fn println_current(s: &str) {
    print_current(&format!("{s}\n"));
}

// def main (args : List String) : IO Unit := do
//   let child ← IO.Process.spawn { cmd := "cat", stdin := .piped, stdout := .inherit }
//   let (stdin, child) ← child.takeStdin
//   let t ← IO.asTask (do
//     discard <| IO.setStdout (IO.FS.Stream.ofHandle stdin)
//     IO.println s!"via cat {args.length}")
//   let _ ← IO.wait t
//   let _d ← IO.asTask (prio := .dedicated) (do
//     let c ← child.wait
//     IO.eprintln s!"cat exited {c}")
//   IO.eprintln "main done"
fn worker_streams_before_dedicated(args: &[String]) -> u32 {
    let n = args.len();
    let child = ok(lio::spawn(
        "cat",
        &[],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    ));
    let stdin = child.stdin.expect("piped");
    let process = child.process.take_stdin();
    let t = as_task(
        move || {
            let _ = lean_runtime::io::streams::set_stdout(Some(stdin) as HandleOut, || None);
            println_current(&format!("via cat {n}"));
        },
        PRIO_DEFAULT,
    );
    t.get();
    drop(t);
    // `_d` is unused: compiled Lean drops it at once (an IO task still runs)
    drop(as_task(
        move || match process.wait() {
            Ok(c) => eprintln(&format!("cat exited {c}")),
            Err(e) => eprintln(&format!("wait failed: {e:?}")),
        },
        PRIO_DEDICATED,
    ));
    eprintln("main done");
    0
}

// ---------------------------------------------------------------------------
// Started pure tasks and sleeping workers (reviews RF3-05, LF3-04, LF3-05).

// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let t0 ← IO.monoMsNow
//   let ticker ← IO.asTask (do IO.sleep 1000; IO.eprintln "tick"; return 1)
//   IO.sleep 50
//   let p := Task.spawn fun _ => slow k
//   let _ ← IO.hasFinished p
//   let t := Task.spawn fun _ => k % 7
//   let v := t.get
//   IO.eprintln s!"t = {v}, before 500 ms: {decide ((← IO.monoMsNow) - t0 < 500)}"
//   let _ ← IO.wait ticker
//   IO.eprintln s!"p = {p.get}"
fn picked_task_short_sleeper_long(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    let t0 = mono_ms_now();
    let ticker = as_task(
        || {
            sleep(1000);
            eprintln("tick");
            1u64
        },
        PRIO_DEFAULT,
    );
    sleep(50);
    let p = Task::spawn(move || slow(k), PRIO_DEFAULT);
    let _ = has_finished(&p);
    let t = Task::spawn(move || k % 7, PRIO_DEFAULT);
    let v = t.get();
    drop(t);
    let before = mono_ms_now() - t0 < 500;
    eprintln(&format!("t = {v}, before 500 ms: {before}"));
    ticker.get();
    eprintln(&format!("p = {}", p.get()));
    0
}

// def main (args : List String) : IO Unit := do
//   let k := args.head!.toNat!
//   let _wd ← IO.asTask (do
//     IO.sleep 3000
//     if !(← IO.checkCanceled) then
//       IO.eprintln "timeout"
//       IO.Process.exit 1)
//   IO.sleep 50
//   let p := Task.spawn fun _ => slow k
//   let _ ← IO.hasFinished p
//   let t := Task.spawn fun _ => k % 7
//   IO.eprintln s!"t = {t.get}"
fn picked_task_watchdog(args: &[String]) -> u32 {
    let k = to_nat(&args[0]);
    // `_wd` is unused: compiled Lean drops it at once (an IO task still runs)
    drop(as_task(
        || {
            sleep(3000);
            if !check_canceled() {
                eprintln("timeout");
                crate::glue::process_exit(1);
            }
        },
        PRIO_DEFAULT,
    ));
    sleep(50);
    let p = Task::spawn(move || slow(k), PRIO_DEFAULT);
    let _ = has_finished(&p);
    let t = Task::spawn(move || k % 7, PRIO_DEFAULT);
    eprintln(&format!("t = {}", t.get()));
    0
}

// partial def spinSleep (x acc : UInt64) : UInt64 :=
//   if x == 0 then acc else
//     let x' := dbgSleep 0 fun _ => x * 6364136223846793005 + 1442695040888963407
//     spinSleep x' (acc + 1)
fn spin_sleep(mut x: u64, mut acc: u64) -> u64 {
    while x != 0 {
        sleep(0);
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        acc = acc.wrapping_add(1);
    }
    acc
}

// def main (args : List String) : IO Unit := do
//   let s := args.head!.toNat!.toUInt64 ||| 1
//   let n := args[1]!.toNat!
//   let p := Task.spawn fun _ => spinSleep s 0
//   let _ ← IO.hasFinished p
//   let q := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
//   let _ ← IO.hasFinished q
//   let t := Task.spawn fun _ => n + 1
//   IO.eprintln s!"t = {t.get}"
//   IO.eprintln s!"p finished: {← IO.hasFinished p}, q finished: {← IO.hasFinished q}"
fn picked_task_sleep_zero(args: &[String]) -> u32 {
    let s = seed(args);
    let n = to_nat(&args[1]);
    let p = Task::spawn(move || spin_sleep(s, 0), PRIO_DEFAULT);
    let _ = has_finished(&p);
    let q = Task::spawn(move || (0..n).sum::<u64>(), PRIO_DEFAULT);
    let _ = has_finished(&q);
    let t = Task::spawn(move || n + 1, PRIO_DEFAULT);
    eprintln(&format!("t = {}", t.get()));
    drop(t);
    let (fp, fq) = (has_finished(&p), has_finished(&q));
    eprintln(&format!("p finished: {fp}, q finished: {fq}"));
    0
}

// ---------------------------------------------------------------------------
// Threads mode, batch T3 (docs/threads.md, section 4): cases that need real
// contention, recorded natively; both drivers run them.

/// `resolveAt ps i v`: resolve promise `i` with `v`.
fn resolve_at(ps: &[Promise<u64>], i: usize, v: u64) {
    if let Some(p) = ps.get(i) {
        p.resolve(v);
    }
}

/// `waitAt ps i`: wait for promise `i`.
fn wait_at(ps: &[Promise<u64>], i: usize) -> u64 {
    match ps.get(i) {
        Some(p) => p.result_opt().get().unwrap_or(0),
        None => 0,
    }
}

// def main (args : List String) : IO Unit := do
//   let n := args.head!.toNat!
//   let ps ← (List.range n).toArray.mapM fun _ => IO.Promise.new (α := Nat)
//   let mut ts := #[]
//   for i in [0:n] do
//     let t ← IO.asTask do
//       if i + 1 == n then
//         IO.println s!"task {i} resolves task {i - 1}'s promise"
//         resolveAt ps (i - 1) 1
//         return 1
//       else
//         let v ← waitAt ps i
//         IO.println s!"task {i} got {v}"
//         if i > 0 then
//           resolveAt ps (i - 1) (v + 1)
//         return v + 1
//     ts := ts.push t
//   match ts[0]? with
//   | some t =>
//     match ← IO.wait t with
//     | .ok v => IO.println s!"main got {v}"
//     | .error e => IO.println s!"error: {e}"
//   | none => IO.println "no task"
fn wait_chain_beyond_pool(args: &[String]) -> u32 {
    let n = to_nat(&args[0]) as usize;
    // the array of promises, one object every task holds
    let ps: Obj<Vec<Promise<u64>>> = Obj::new((0..n).map(|_| Promise::new()).collect());
    let mut ts = Vec::new();
    for i in 0..n {
        let ps = ps.clone();
        let t = as_task(
            move || {
                if i + 1 == n {
                    // `i - 1` on `Nat`
                    let prev = i.saturating_sub(1);
                    println(&format!("task {i} resolves task {prev}'s promise"));
                    resolve_at(&ps, prev, 1);
                    1u64
                } else {
                    let v = wait_at(&ps, i);
                    println(&format!("task {i} got {v}"));
                    if i > 0 {
                        resolve_at(&ps, i - 1, v + 1);
                    }
                    v + 1
                }
            },
            PRIO_DEFAULT,
        );
        ts.push(t);
    }
    // `ps`'s last use was the loop's
    drop(ps);
    match ts.first() {
        Some(t) => println(&format!("main got {}", t.get())),
        None => println("no task"),
    }
    0
}

// def main (args : List String) : IO Unit := do
//   let slowMs := args[0]!.toNat!
//   let fastMs := args[1]!.toNat!
//   let slow ← IO.asTask do
//     IO.sleep slowMs.toUInt32
//     return "slow"
//   let fast ← IO.asTask do
//     IO.sleep fastMs.toUInt32
//     return "fast"
//   match ← IO.waitAny [slow, fast] with
//   | .ok s => IO.println s!"waitAny: {s}"
//   | .error e => IO.println s!"error: {e}"
//   IO.println s!"slow finished then: {← IO.hasFinished slow}"
//   match ← IO.wait slow with
//   | .ok s => IO.println s!"then {s}"
//   | .error e => IO.println s!"error: {e}"
fn wait_any_faster(args: &[String]) -> u32 {
    let slow_ms = to_nat(&args[0]) as u32;
    let fast_ms = to_nat(&args[1]) as u32;
    let slow = as_task(
        move || {
            sleep(slow_ms);
            "slow".to_string()
        },
        PRIO_DEFAULT,
    );
    let fast = as_task(
        move || {
            sleep(fast_ms);
            "fast".to_string()
        },
        PRIO_DEFAULT,
    );
    // the list is `fast`'s last use
    let s = wait_any(&[slow.clone(), fast]);
    println(&format!("waitAny: {s}"));
    println(&format!("slow finished then: {}", has_finished(&slow)));
    println(&format!("then {}", slow.get()));
    0
}

// LB-13 while tasks still enqueue. The twin gives the correct outcome, which
// the case expects (native's is in its `native` field).
//
// def main (args : List String) : IO Unit := do
//   let busyMs := args[0]!.toNat!
//   let stepMs := args[1]!.toNat!
//   let n := args[2]!.toNat!
//   let _ ← IO.asTask do
//     IO.sleep busyMs.toUInt32
//     IO.println "busy task done"
//   let _ ← IO.asTask (prio := .dedicated) do
//     for i in [0:n] do
//       IO.sleep stepMs.toUInt32
//       IO.println s!"dedicated enqueues task {i}"
//       let _ ← IO.asTask (IO.println s!"late task {i} ran")
//   IO.println "main returns"
fn late_tasks_while_enqueuing(args: &[String]) -> u32 {
    let busy_ms = to_nat(&args[0]) as u32;
    let step_ms = to_nat(&args[1]) as u32;
    let n = to_nat(&args[2]);
    let _ = as_task(
        move || {
            sleep(busy_ms);
            println("busy task done");
        },
        PRIO_DEFAULT,
    );
    let _ = as_task(
        move || {
            for i in 0..n {
                sleep(step_ms);
                println(&format!("dedicated enqueues task {i}"));
                let _ = as_task(move || println(&format!("late task {i} ran")), PRIO_DEFAULT);
            }
        },
        PRIO_DEDICATED,
    );
    println("main returns");
    0
}
