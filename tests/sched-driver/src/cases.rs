//! Rust-level ports of `tests/cases/{tasks,sync,refs}/*.lean`, line by line,
//! as a translator's output would read: the same tasks, values and effects in
//! the same order, and each value dropped where compiled Lean releases it.

use crate::glue::{eprintln, println};
use crate::lean::*;
use lean_runtime::sched::sync::{Condvar, Mutex, RecursiveMutex, SharedMutex};
use std::cell::RefCell;
use std::rc::Rc;

/// A case: its initializer (Lean's `initialize` declarations) and `main`.
pub type Case = (fn(), fn(&[String]) -> u32);

/// The cases, by id.
pub fn lookup(id: &str) -> Option<Case> {
    fn no_init() {}
    Some(match id {
        "checkcanceled_after_main" => (no_init, checkcanceled_after_main),
        "dropped_pure_task" => (no_init, dropped_pure_task),
        "exit_joins_before_flush" => (no_init, exit_joins_before_flush),
        "hasfinished_spin" => (no_init, hasfinished_spin),
        "runaway_io_task_unawaited" => (no_init, runaway_io_task_unawaited),
        "runaway_pure_task_referenced" => (init_keep, runaway_pure_task_referenced),
        "runaway_pure_task_started" => (no_init, runaway_pure_task_started),
        "sleep_polling_spin" => (no_init, sleep_polling_spin),
        "sync_dependent_order" => (no_init, sync_dependent_order),
        "promise_across_tasks" => (no_init, promise_across_tasks),
        "stack_overflow_in_task" => (no_init, stack_overflow_in_task),
        "mutex_handoff" => (no_init, mutex_handoff),
        "condvar_turns" => (no_init, condvar_turns),
        "shared_mutex_readers" => (no_init, shared_mutex_readers),
        "recursive_mutex" => (no_init, recursive_mutex),
        "pure_chain_io_dep" => (no_init, pure_chain_io_dep),
        "pure_bind_io_dep" => (no_init, pure_bind_io_dep),
        "exit_from_task" => (no_init, exit_from_task),
        "get_in_sync_task" => (no_init, get_in_sync_task),
        "late_task_after_main" => (no_init, late_task_after_main),
        "late_dependent_of_dedicated" => (no_init, late_dependent_of_dedicated),
        "late_wait_dedicated" => (no_init, late_wait_dedicated),
        "late_wait_pool" => (no_init, late_wait_pool),
        "late_pool_child_runs" => (no_init, late_pool_child_runs),
        "late_dedicated_child_runs" => (no_init, late_dedicated_child_runs),
        "main_waits_dedicated_child" => (no_init, main_waits_dedicated_child),
        "wait_dep_mid_walk" => (no_init, wait_dep_mid_walk),
        "wait_dep_mid_promise_walk" => (no_init, wait_dep_mid_promise_walk),
        "poll_dep_mid_walk" => (no_init, poll_dep_mid_walk),
        "wait_any_own_dep" => (no_init, wait_any_own_dep),
        "task_waits_own_dep" => (no_init, task_waits_own_dep),
        "sync_dep_waits_older" => (no_init, sync_dep_waits_older),
        "lost_update" => (no_init, lost_update),
        "set_during_modify" => (no_init, set_during_modify),
        "get_during_modify" => (no_init, get_during_modify),
        "swap_during_modify" => (no_init, swap_during_modify),
        // tests/cases/taskio: blocking IO in programs with tasks (sched-io)
        "output_big_stdout" => (no_init, output_big_stdout),
        "output_both_overflow" => (no_init, output_both_overflow),
        "task_reads_main_writes" => (no_init, task_reads_main_writes),
        "wait_in_task" => (no_init, wait_in_task),
        "output_while_ticking" => (no_init, output_while_ticking),
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
        "signal_rearm_in_sync_dependent" => (no_init, signal_rearm_in_dependent),
        "signal_rearm_in_async_dependent" => (no_init, signal_rearm_in_dependent),
        // tests/cases/io: the cases with tasks
        "lock_blocked" => (no_init, lock_blocked),
        "lock_exit" => (no_init, lock_exit),
        "lock_during_read" => (no_init, lock_during_read),
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
        // Not a Lean program: a Rust panic (a translator's or the runtime's
        // bug) in a task on a context of its own.
        "rust_panic_in_task" => (no_init, rust_panic_in_task),
        // Not Lean programs either: leanrs's adversarial checks of
        // docs/sched.md's "Why Glue::suspend is sound" (S5, S6).
        "adv_block_in_drop_during_unwind" => (no_init, adv_block_in_drop_during_unwind),
        "adv_panic_in_sync_dep_of_drop" => (no_init, adv_panic_in_sync_dep_of_drop),
        "adv_exit_from_task" => (no_init, adv_exit_from_task),
        // tests/cases/net: networking (net-1)
        _ => return crate::netcases::lookup(id),
    })
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
thread_local! {
    static KEEP: RefCell<Option<Ref<Option<Task<u64>>>>> = const { RefCell::new(None) };
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
    // `IO.Promise.result!`: `result?.map (sync := true) Option.getOrBlock!`.
    let result = map_task(
        |o: Option<u64>| o.expect("resolved"),
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        false,
    );
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
fn deep(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        std::hint::black_box(deep(std::hint::black_box(n - 1))) + 1
    }
}

fn stack_overflow_in_task(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    println("main starts");
    let _ = as_task(move || println(&format!("depth {}", deep(n))), PRIO_DEFAULT);
    sleep(1000);
    eprintln("not reached");
    0
}

fn rust_panic_in_task(_: &[String]) -> u32 {
    println("main starts");
    let _ = as_task(|| -> () { panic!("a Rust panic in a task") }, PRIO_DEFAULT);
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
    let m = Rc::new(Mutex::new());
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
    m: Rc<Mutex>,
    cv: Rc<Condvar>,
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
    let m = Rc::new(Mutex::new());
    let cv = Rc::new(Condvar::new());
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
fn reader(s: Rc<SharedMutex>, inside: Rc<Promise<()>>, leave: Rc<Promise<()>>) {
    s.read();
    inside.resolve(());
    leave.result_opt().get();
    s.unlock_read();
}

fn shared_mutex_readers(args: &[String]) -> u32 {
    let ms = to_nat(&args[0]) as u32;
    let s = Rc::new(SharedMutex::new());
    let p1 = Rc::new(Promise::new());
    let p2 = Rc::new(Promise::new());
    let leave = Rc::new(Promise::new());
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
    let r = Rc::new(RecursiveMutex::new());
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
// tests/cases/taskio and the io cases with tasks: blocking IO on one thread
// (sched-io).

use crate::lio::{self, quote, R};
use lean_runtime::io::process::{Stdio, StdioConfig};
use lean_runtime::io::{FsMode, Handle};

/// An uncaught `IO.Error` ends a Lean `main`: the twins expect none.
fn ok<T>(r: R<T>) -> T {
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
// `sync` dependent of each tick (the twin skips the `none` resolution of a
// dropped promise: in Lean the dependent's task is the promise itself, so
// it is never dropped while the dependent waits).
fn catchup_arm(t: UTimer, n: Ref<u64>, work_ms: u64) {
    let p = t.next(UvPromise::new);
    let task = p.result_opt();
    drop(p);
    let _ = map_task(
        move |v: Option<()>| {
            if v.is_none() {
                return;
            }
            spin_ms(work_ms);
            n.modify(|k| k + 1);
            catchup_arm(t, n, work_ms);
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
