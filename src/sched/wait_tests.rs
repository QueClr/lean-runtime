//! Unit tests of the wait cores (batch wait-1) on the single-thread
//! scheduler: `wait.rs` (core 3.1), `refs.rs` (core 3.2) and `drain.rs`
//! (core 3.3, whose test bodies `sched/mt/tests.rs` runs in threads mode
//! too). Each test runs on a thread of its own, so with a scheduler of its
//! own. Only `main`'s context blocks here (the crate cannot hold the glue's
//! `unsafe` suspend step); the programs where several contexts wait are in
//! `tests/sched-driver` (`src/wait1.rs`). Tests that start a context switch
//! stacks, which Miri cannot run; the others run under Miri.

use super::drain::tests as drain_bodies;
use super::refs::{frame, ref_keyed};
use super::wait::tests::{keyed_entry, keyed_len, set_keyed_runner, set_runner};
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

/// The message of a caught panic.
fn message(e: Box<dyn std::any::Any + Send>) -> String {
    match e.downcast::<String>() {
        Ok(s) => *s,
        Err(e) => e
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Core 3.1: Gate, WaitList, the keyed functions

#[test]
fn a_gate_runs_once_then_stores_after_the_writers_point() {
    let g = Gate::new();
    let key = std::ptr::addr_of!(g) as usize;
    let value = Cell::new(None);
    assert!(!g.running());
    assert_eq!(g.step(key), Step::Run);
    assert!(g.running());
    // a gate's runner is in the gate: no entry in the keyed table
    assert_eq!(keyed_len(), 0);
    g.finish(key, || value.set(Some(5)));
    assert_eq!(value.get(), Some(5));
    assert!(!g.running());
    assert_eq!(keyed_len(), 0);
    // `new` is a `const fn`, and the default is a gate with no runner
    let g = const { Gate::new() };
    assert!(!g.running() && !Gate::default().running());
}

/// W3: the runner's own step (a computation that needs itself) inside a
/// no-suspend scope is a Rust panic with the reason, not a hang.
#[test]
fn a_self_force_inside_a_no_suspend_scope_panics() {
    let g = Gate::new();
    assert_eq!(g.step(8), Step::Run);
    let _scope = no_suspend();
    let e = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.step(8)))
        .expect_err("W3 panics");
    assert_eq!(message(e), WAIT_IN_NO_SUSPEND);
}

/// W3: a wait for another context's run inside a no-suspend scope is a
/// Rust panic, and registers nothing.
#[test]
fn a_gate_wait_inside_a_no_suspend_scope_panics() {
    let g = Gate::new();
    set_runner(&g, 1000);
    let _scope = no_suspend();
    let e = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.step(16)))
        .expect_err("W3 panics");
    assert_eq!(message(e), WAIT_IN_NO_SUSPEND);
    assert_eq!(keyed_len(), 0);
}

#[test]
fn a_wait_list_wait_inside_a_no_suspend_scope_panics() {
    let w = WaitList::new();
    let _scope = no_suspend();
    let e =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.wait())).expect_err("W3 panics");
    assert_eq!(message(e), WAIT_IN_NO_SUSPEND);
    assert!(w.is_empty());
    // a wake with no waiter does nothing, also inside the scope
    w.wake_all();
}

/// L8: an object's key is its address, so it is even; debug builds check
/// it.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "even")]
fn an_odd_key_for_a_gate_is_a_debug_assertion() {
    let g = Gate::new();
    g.step(9);
}

/// The keyed claims of lean2rr's constant slots, keyed `(slot << 1) | 1`:
/// the runner is recorded in the table until the store.
#[test]
fn keyed_claims_with_odd_keys() {
    let key = |slot: usize| (slot << 1) | 1;
    assert!(step_keyed(key(5)));
    assert_eq!(keyed_entry(key(5)), Some((Some(MAIN.index()), vec![])));
    assert_eq!(keyed_len(), 1);
    assert!(step_keyed(key(6)));
    assert_eq!(keyed_len(), 2);
    done_keyed(key(5));
    assert_eq!(keyed_len(), 1);
    assert_eq!(keyed_entry(key(5)), None);
    // a store with no entry does nothing
    done_keyed(key(5));
    done_keyed(16);
    assert_eq!(keyed_len(), 1);
    done_keyed(key(6));
    assert_eq!(keyed_len(), 0);
    // a slot whose value went (a translator resets nothing, but the table
    // allows a claim again)
    assert!(step_keyed(key(5)));
    done_keyed(key(5));
    assert_eq!(keyed_len(), 0);
}

/// `main` waits for a gate whose runner is another context (here a
/// context that has ended, since the unit tests cannot suspend one), and
/// the store of a task on a context of its own wakes it: `Step::Again`,
/// then the value.
#[test]
#[cfg_attr(miri, ignore)]
fn main_waits_for_another_contexts_run_and_wakes_at_its_store() {
    start_test(1);
    let g = Rc::new(Gate::new());
    let key = Rc::as_ptr(&g) as usize;
    let value = Rc::new(Cell::new(None));
    set_runner(&g, 1000);
    let (g2, v2) = (g.clone(), value.clone());
    let _t = spawn(
        Box::new(move || {
            g2.finish(key, || v2.set(Some(7)));
            Outcome::Done
        }),
        0,
        true,
    );
    let mut steps = Vec::new();
    while value.get().is_none() {
        steps.push(g.step(key));
    }
    assert_eq!(steps, [Step::Again]);
    assert_eq!(value.get(), Some(7));
    assert_eq!(keyed_len(), 0);
    finish();
}

/// The same with the keyed claim: `main`'s `step_keyed` waits for the
/// claim another context holds (recorded by hand: a runner that blocked,
/// which the unit tests cannot suspend), and `done_keyed` wakes it.
#[test]
#[cfg_attr(miri, ignore)]
fn a_keyed_wait_wakes_at_done_keyed() {
    start_test(1);
    let key = (3 << 1) | 1;
    set_keyed_runner(key, 1000);
    let set = Rc::new(Cell::new(false));
    let s2 = set.clone();
    let _store = spawn(
        Box::new(move || {
            s2.set(true);
            done_keyed(key);
            Outcome::Done
        }),
        9,
        true,
    );
    let mut answers = Vec::new();
    while !set.get() {
        answers.push(step_keyed(key));
    }
    assert_eq!(answers, [false], "one wait, never a claim");
    assert_eq!(keyed_len(), 0);
    finish();
}

// ---------------------------------------------------------------------------
// Core 3.2: the single-thread Ref

/// A value whose live copies are counted.
struct Counted(u64, Rc<Cell<i64>>);

impl Counted {
    fn new(v: u64, live: &Rc<Cell<i64>>) -> Counted {
        live.set(live.get() + 1);
        Counted(v, live.clone())
    }
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        Counted::new(self.0, &self.1)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.1.set(self.1.get() - 1);
    }
}

#[test]
fn ref_operations_and_every_value_dropped_once() {
    let live = Rc::new(Cell::new(0));
    let c = |v| Counted::new(v, &live);
    {
        let r = Ref::new(c(1));
        assert_eq!(r.get().0, 1);
        r.set(c(2));
        assert_eq!(r.swap(c(3)).0, 2);
        let v = r.take();
        assert_eq!(v.0, 3);
        r.put(c(v.0 + 1));
        drop(v);
        assert_eq!(r.get().0, 4);
        r.modify(|v| c(v.0 + 1));
        assert_eq!(r.get().0, 5);
        let k = r.modify_get(|v| (v.0 * 10, c(v.0 + 1)));
        assert_eq!(k, 50);
        assert_eq!(r.get().0, 6);
        assert_eq!(live.get(), 1);
        // a placeholder: never read; a put fills it
        let e: Ref<Counted> = Ref::empty();
        e.put(c(7));
        assert_eq!(e.get().0, 7);
        assert_eq!(live.get(), 2);
    }
    assert_eq!(live.get(), 0);
}

/// Take and put move the value: a `Vec` keeps its buffer.
#[test]
fn ref_modify_is_in_place() {
    let r = Ref::new((0..1000u64).collect::<Vec<u64>>());
    let v = r.take();
    let p0 = v.as_ptr();
    r.put(v);
    for i in 0..100u64 {
        r.modify(|mut v| {
            v[i as usize] += 1;
            v
        });
    }
    let v = r.take();
    assert_eq!(v.as_ptr(), p0, "the buffer stayed in place");
    assert_eq!((v[0], v[99], v[100]), (1, 100, 100));
    r.put(v);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "put into a full reference")]
fn a_put_into_a_full_reference_is_a_debug_assertion() {
    Ref::new(1).put(2);
}

/// W3: the taker's own `get` (or anyone's) of an empty reference inside a
/// no-suspend scope is a Rust panic, not a wait.
#[test]
fn a_get_of_a_taken_reference_inside_a_no_suspend_scope_panics() {
    let r = Ref::new(1);
    let v = r.take();
    let _scope = no_suspend();
    let e =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.get())).expect_err("W3 panics");
    assert_eq!(message(e), WAIT_IN_NO_SUSPEND);
    r.put(v);
}

/// The keyed form: `take` records the running frame; the store in the
/// same frame is the closing store; `put` closes too.
#[test]
fn keyed_take_then_the_closing_store_in_the_same_frame() {
    let key = 64;
    assert!(!ref_keyed::read_point());
    ref_keyed::take(key);
    assert!(ref_keyed::is_taken(key));
    assert!(ref_keyed::read_point() && ref_keyed::write_point() && ref_keyed::swap_point());
    // another reference is not taken: no wait
    ref_keyed::wait(128);
    ref_keyed::store(128);
    assert!(ref_keyed::is_taken(key));
    ref_keyed::store(key);
    assert!(!ref_keyed::is_taken(key));
    assert!(!ref_keyed::write_point());
    // a store with nothing taken returns at once
    ref_keyed::store(key);
    ref_keyed::take(key);
    ref_keyed::put(key);
    assert!(!ref_keyed::is_taken(key));
}

/// The frame: `main`'s outside any task is the same before and after the
/// task manager starts; a task run on `main`'s stack (a `LEAN_SYNC_PRIO`
/// task, as a `sync` dependent) runs at a deeper frame, so a store there
/// is not `modify`'s closing store; the frame is the same again after it.
#[test]
fn a_task_run_inside_modify_has_a_deeper_frame() {
    let before = frame();
    start_test(1);
    let outside = frame();
    assert_eq!(before, outside);
    ref_keyed::take(256);
    let inner = Rc::new(RefCell::new(None));
    let i2 = inner.clone();
    let _t = spawn(
        Box::new(move || {
            *i2.borrow_mut() = Some(frame());
            Outcome::Done
        }),
        u32::MAX as u64,
        true,
    );
    let inner = inner.borrow().expect("the task ran at once");
    assert_ne!(inner, outside);
    assert_eq!(frame(), outside);
    ref_keyed::store(256);
    assert!(!ref_keyed::is_taken(256));
    finish();
}

// ---------------------------------------------------------------------------
// Core 3.3: the deferred resolutions (the bodies are in `drain.rs`)

#[test]
fn drain_resolutions_run_after_the_drain_in_drop_order() {
    start_test(1);
    drain_bodies::resolutions_run_after_the_drain_in_drop_order();
}

#[test]
fn drain_a_nested_drain_resolves_its_own_promises_first() {
    start_test(1);
    drain_bodies::a_nested_drain_resolves_its_own_promises_first();
}

#[test]
fn drain_entries_moved_out_still_count_as_pending() {
    start_test(1);
    drain_bodies::entries_moved_out_still_count_as_pending();
}

#[test]
fn drain_a_panic_through_a_drain_leaves_the_entries_queued() {
    start_test(1);
    drain_bodies::a_panic_through_a_drain_leaves_the_entries_queued();
}

#[test]
fn drain_a_panic_out_of_an_entry_requeues_the_rest() {
    start_test(1);
    drain_bodies::a_panic_out_of_an_entry_requeues_the_rest();
}

#[test]
fn drain_run_deferred_inside_a_scope() {
    start_test(1);
    drain_bodies::run_deferred_inside_a_scope();
}

#[test]
fn drain_resolve_inside_a_scope() {
    start_test(1);
    drain_bodies::resolve_inside_a_scope();
}

#[test]
fn drain_scopes_nest() {
    start_test(1);
    drain_bodies::scopes_nest();
}

/// R6: a deferred resolution still queued at a context switch is a debug
/// assertion (here `main` blocks with one queued outside any drain). The
/// entry stays queued when the thread ends, and the list, which has no
/// destructor, leaks it by design: ignored under Miri.
#[cfg(debug_assertions)]
#[test]
#[cfg_attr(miri, ignore)]
#[should_panic(expected = "R6")]
fn drain_an_entry_queued_at_a_switch_is_a_debug_assertion() {
    start_test(1);
    defer(Deferred::Call(Box::new(|| {})));
    block_sync();
}

/// leanrs's model test of its drain (`beh_drain_defers_promise.rs`, 2000
/// random value graphs; Lem-Deep (6)), against the real `DrainScope`,
/// `defer`, `run_deferred` and the scheduler's `resolve`, whose `sync`
/// dependents run on `main`'s stack. A value graph holds leaves, promises,
/// plain nodes and `Deep` nodes (a node whose drop starts a drain, or joins
/// the running one's worklist). Each promise has one `sync` dependent,
/// which does nothing, drops another value graph (a new drain), or hangs.
/// A hang is modelled as a panic out of the resolution (the hanging context
/// never continues): the entries the walk had not run are discarded at the
/// seed's end, as they would stay on that context's stack. Checked: no
/// resolution inside a drain or the scope; each promise resolved once;
/// every dropped promise resolved unless its context hung; every leaf
/// dropped at most once; after every root the depth is 0 and the scope
/// left; after a hang, another drop on the thread drains in full.
///
/// Under Miri it leaks by design: the values a hung context keeps are
/// forgotten, and so are the jobs of the dependents of the promises it
/// never resolves (the scheduler forgets pending jobs at thread exit,
/// `Tasks`'s `Drop`). So it is ignored there by default; run it with
/// `MIRIFLAGS=-Zmiri-ignore-leaks cargo miri test --lib -- --include-ignored
/// sched::wait_tests::drain_model` (10 graphs under Miri).
#[test]
#[cfg_attr(miri, ignore)]
fn drain_model_of_random_value_graphs() {
    start_test(1);
    #[derive(Debug, Clone, PartialEq)]
    enum Event {
        Dropped(u32),
        Resolved(u32, bool),
        PromiseLast(u32),
    }
    enum Dep {
        Nothing,
        DropValue(Val),
        Hang,
    }
    struct Hung;
    #[allow(dead_code)]
    enum Val {
        Leaf(Leaf),
        Promise(Promise),
        Deep(Deep),
        Node(Box<Node>),
    }
    #[allow(dead_code)]
    struct Node(Vec<Val>);
    struct Leaf(u32);
    struct Promise(u32, TaskId);
    struct Deep(Option<Box<Node>>);
    thread_local! {
        static LOG: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
        static DEPS: RefCell<Vec<Option<Dep>>> = const { RefCell::new(Vec::new()) };
        /// A `Deep` drain's worklist (the translator's own).
        static QUEUE: RefCell<Vec<Node>> = const { RefCell::new(Vec::new()) };
        static DRAINING: Cell<bool> = const { Cell::new(false) };
    }
    impl Drop for Leaf {
        fn drop(&mut self) {
            LOG.with(|l| l.borrow_mut().push(Event::Dropped(self.0)));
        }
    }
    impl Drop for Promise {
        fn drop(&mut self) {
            LOG.with(|l| l.borrow_mut().push(Event::PromiseLast(self.0)));
            let id = self.1;
            if DrainScope::active() {
                // both kinds of entry: lean2rr's and leanrs's
                if self.0.is_multiple_of(2) {
                    defer(Deferred::Resolve(id));
                } else {
                    defer(Deferred::Call(Box::new(move || {
                        resolve(id, || {});
                    })));
                }
            } else {
                resolve(id, || {});
            }
        }
    }
    impl Drop for Deep {
        fn drop(&mut self) {
            let Some(n) = self.0.take() else { return };
            if DRAINING.with(Cell::get) {
                QUEUE.with(|q| q.borrow_mut().push(*n));
                return;
            }
            let scope = DrainScope::enter();
            DRAINING.with(|d| d.set(true));
            drop(n);
            while let Some(n) = QUEUE.with(|q| q.borrow_mut().pop()) {
                drop(n);
            }
            DRAINING.with(|d| d.set(false));
            drop(scope);
        }
    }
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }
    fn promise(id: u32) -> TaskId {
        let p = promise_new().expect("the task manager runs");
        depend(
            p,
            Box::new(move || {
                let inside = DrainScope::active() || in_no_suspend();
                LOG.with(|l| l.borrow_mut().push(Event::Resolved(id, inside)));
                let dep = DEPS.with(|d| d.borrow_mut().get_mut(id as usize).and_then(Option::take));
                match dep {
                    Some(Dep::DropValue(v)) => drop(v),
                    Some(Dep::Hang) => std::panic::panic_any(Hung),
                    Some(Dep::Nothing) | None => {}
                }
                Outcome::Done
            }),
            0,
            true,
            true,
        );
        p
    }
    fn gen(rng: &mut Rng, depth: u32, next: &Cell<u32>, hang_ok: &Cell<bool>) -> Val {
        let id = next.get();
        next.set(id + 1);
        let k = if depth == 0 {
            rng.below(2)
        } else {
            rng.below(4)
        };
        match k {
            0 => Val::Leaf(Leaf(id)),
            1 => {
                let dep = match rng.below(4) {
                    0 if depth > 0 => Dep::DropValue(gen(rng, depth - 1, next, hang_ok)),
                    1 if hang_ok.get() && rng.below(3) == 0 => {
                        hang_ok.set(false);
                        Dep::Hang
                    }
                    _ => Dep::Nothing,
                };
                DEPS.with(|d| {
                    let mut d = d.borrow_mut();
                    if d.len() <= id as usize {
                        d.resize_with(id as usize + 1, || None);
                    }
                    d[id as usize] = Some(dep);
                });
                Val::Promise(Promise(id, promise(id)))
            }
            2 => Val::Deep(Deep(Some(Box::new(Node(
                (0..rng.below(4))
                    .map(|_| gen(rng, depth - 1, next, hang_ok))
                    .collect(),
            ))))),
            _ => Val::Node(Box::new(Node(
                (0..rng.below(4))
                    .map(|_| gen(rng, depth - 1, next, hang_ok))
                    .collect(),
            ))),
        }
    }
    let seeds: u64 = if cfg!(miri) { 10 } else { 2000 };
    let (mut hangs, mut deferred) = (0u32, 0u32);
    for seed in 1..=seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
        // the dependents a hung context's promises still hold stay held
        std::mem::forget(DEPS.with(|d| std::mem::take(&mut *d.borrow_mut())));
        LOG.with(|l| l.borrow_mut().clear());
        let next = Cell::new(0u32);
        let hang_ok = Cell::new(true);
        let root = Val::Deep(Deep(Some(Box::new(Node(
            (0..4).map(|_| gen(&mut rng, 4, &next, &hang_ok)).collect(),
        )))));
        let hung = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(root))).is_err();
        hangs += hung as u32;
        if hung {
            // the hung walk's other entries stay on its context's stack
            DRAINING.with(|d| d.set(false));
            std::mem::forget(QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut())));
            super::drain::tests::discard_queued();
        }
        assert!(!DrainScope::active(), "seed {seed}: a drain left active");
        assert!(!in_no_suspend(), "seed {seed}: the scope left entered");
        assert!(!deferred_pending(), "seed {seed}: an entry left pending");
        let log = LOG.with(|l| l.borrow().clone());
        for e in &log {
            if let Event::Resolved(id, inside) = e {
                assert!(!inside, "seed {seed}: promise {id} resolved inside a drain");
            }
        }
        let lasts: Vec<u32> = log
            .iter()
            .filter_map(|e| match e {
                Event::PromiseLast(i) => Some(*i),
                _ => None,
            })
            .collect();
        let res: Vec<u32> = log
            .iter()
            .filter_map(|e| match e {
                Event::Resolved(i, _) => Some(*i),
                _ => None,
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for r in &res {
            assert!(seen.insert(*r), "seed {seed}: promise {r} resolved twice");
        }
        if !hung {
            assert_eq!(
                lasts.len(),
                res.len(),
                "seed {seed}: a dropped promise never resolved"
            );
        }
        for (k, e) in log.iter().enumerate() {
            if let Event::PromiseLast(i) = e {
                if log.get(k + 1) != Some(&Event::Resolved(*i, false)) && res.contains(i) {
                    deferred += 1;
                }
            }
        }
        let mut ds = std::collections::HashSet::new();
        for e in &log {
            if let Event::Dropped(i) = e {
                assert!(ds.insert(*i), "seed {seed}: leaf {i} dropped twice");
            }
        }
        if hung {
            LOG.with(|l| l.borrow_mut().clear());
            drop(Val::Deep(Deep(Some(Box::new(Node(vec![
                Val::Leaf(Leaf(900_000)),
                Val::Deep(Deep(Some(Box::new(Node(vec![Val::Leaf(Leaf(900_001))]))))),
            ]))))));
            let log = LOG.with(|l| l.borrow().clone());
            assert!(
                log.contains(&Event::Dropped(900_000)) && log.contains(&Event::Dropped(900_001)),
                "seed {seed}: a later drop stranded"
            );
        }
    }
    if !cfg!(miri) {
        assert!(
            hangs > 50 && deferred > 500,
            "coverage: {hangs} hangs, {deferred} deferred resolutions"
        );
    }
}

// ---------------------------------------------------------------------------
// Review RW1 of wait-1: the repros, as regression tests of the fixes

/// RW1-02: a `DrainScope` held across a switch (a glue bug: the
/// scheduler's own waits still suspend in the scope) no longer leaks its
/// depth into the other contexts: the switch sets it aside with the
/// no-suspend depth, so another context is neither in the drain nor in the
/// scope.
#[test]
#[cfg_attr(miri, ignore)]
fn rw1_drain_depth_leaks_across_a_switch() {
    start_test(1);
    let seen = Rc::new(Cell::new(None));
    let s2 = seen.clone();
    let _t = spawn(
        Box::new(move || {
            s2.set(Some((DrainScope::active(), in_no_suspend())));
            Outcome::Done
        }),
        0,
        true,
    );
    {
        let _d = DrainScope::enter();
        sleep_ms(5);
        assert!(DrainScope::active() && in_no_suspend(), "back in its drain");
    }
    assert_eq!(seen.get(), Some((false, false)));
    finish();
}

/// RW1-03: at thread teardown, a drain in a thread-local destructor that
/// runs after the others defers into the list as usual (it has no
/// destructor), and the scope's end runs the entry outside the scope (R3,
/// R4), where it ran at once inside the scope before.
#[test]
fn rw1_defer_at_teardown_runs_inside_the_scope() {
    use std::sync::{Arc, Mutex};
    let out: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let o2 = out.clone();
    std::thread::spawn(move || {
        struct Late(Arc<Mutex<Option<bool>>>);
        impl Drop for Late {
            fn drop(&mut self) {
                let _d = DrainScope::enter();
                let o = self.0.clone();
                defer(Deferred::Call(Box::new(move || {
                    *o.lock().unwrap() = Some(super::reactor::in_no_suspend_scope());
                })));
            }
        }
        thread_local! {
            static LATE: RefCell<Option<Late>> = const { RefCell::new(None) };
        }
        // registered first, so destroyed after the thread's other locals
        LATE.with(|l| *l.borrow_mut() = Some(Late(o2)));
        defer(Deferred::Call(Box::new(|| {})));
        run_deferred();
    })
    .join()
    .unwrap();
    assert_eq!(
        *out.lock().unwrap(),
        Some(false),
        "ran after the drain, outside the scope"
    );
}

/// RW1-01: an entry left queued by a drain whose end the translator did not
/// report (lean2rr without Reussir's patch 0040) belongs to the context
/// that deferred it: `main`'s drains do not run it; it runs on its own
/// context, at the latest when that context ends, and debug builds report
/// it there (R6 when a context ends).
#[test]
#[cfg_attr(miri, ignore)]
fn rw1_entry_left_queued_runs_on_another_context() {
    start_test(1);
    let ran_on = Rc::new(Cell::new(None));
    let deferred_on = Rc::new(Cell::new(None));
    let (r2, d2) = (ran_on.clone(), deferred_on.clone());
    let _t = spawn(
        Box::new(move || {
            d2.set(Some(current_context()));
            let r3 = r2.clone();
            // a drain whose end is not reported: no `run_deferred`
            defer(Deferred::Call(Box::new(move || {
                r3.set(Some(current_context()))
            })));
            Outcome::Done
        }),
        0,
        true,
    );
    let r = std::panic::catch_unwind(|| sleep_ms(5));
    let reported = match r {
        Err(e) => {
            assert!(message(e).contains("(R6)"));
            true
        }
        Ok(()) => false,
    };
    assert_eq!(
        reported,
        cfg!(debug_assertions),
        "R6 at the context's end, in debug builds"
    );
    assert!(deferred_on.get().is_some());
    assert_eq!(ran_on.get(), deferred_on.get(), "ran on its own context");
    assert!(!deferred_pending());
    {
        let _d = DrainScope::enter();
    }
    assert_eq!(ran_on.get(), deferred_on.get());
}

/// RW1-01: entries queued while an entry of a walk runs, by a drain whose
/// end was not reported, run right after that entry, before the walk's
/// next: `pC` before `pB` since `pA` before `pB` (lean2rr's step-4
/// `run_later` shape).
#[test]
fn rw1_unreported_inner_drain_entries_go_behind_the_walk() {
    let log = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    let (l1, l2) = (log.clone(), log.clone());
    defer(Deferred::Call(Box::new(move || {
        let l3 = l1.clone();
        // pA's dependent frees pC in a drain whose end is not reported
        defer(Deferred::Call(Box::new(move || l3.borrow_mut().push("C"))));
        l1.borrow_mut().push("A");
    })));
    defer(Deferred::Call(Box::new(move || l2.borrow_mut().push("B"))));
    run_deferred();
    assert_eq!(*log.borrow(), ["A", "C", "B"]);
    assert!(!deferred_pending());
}
