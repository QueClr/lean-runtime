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
        // Not a Lean program: a Rust panic (a translator's or the runtime's
        // bug) in a task on a context of its own.
        "rust_panic_in_task" => (no_init, rust_panic_in_task),
        // Not Lean programs either: leanrs's adversarial checks of
        // docs/sched.md's "Why Glue::suspend is sound" (S5, S6).
        "adv_block_in_drop_during_unwind" => (no_init, adv_block_in_drop_during_unwind),
        "adv_panic_in_sync_dep_of_drop" => (no_init, adv_panic_in_sync_dep_of_drop),
        "adv_exit_from_task" => (no_init, adv_exit_from_task),
        _ => return None,
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
