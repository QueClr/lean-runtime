//! Programs over the wait cores (batch wait-1; docs/sched.md, "The wait
//! cores") where several contexts wait, which the crate's unit tests cannot
//! run (only `main`'s context blocks there): a translator's thunk and
//! static over `sched::Gate` (leanrs's shape), constants and `busy` thunks
//! over the keyed functions (lean2rr's shape), the keyed `ST.Ref` and its
//! frame rule, 3000 random schedules of `ST.Ref` operations against Lean
//! 4.35's store model (leanrs's evidence `beh_ref_empty_cell.rs`, ported to
//! the real types), and a deferred resolution that hangs. Not Lean
//! programs: `tests/cases.rs` runs them (`w1_*`) and checks their output.
//!
//! A program whose correct outcome is a context that waits forever has the
//! watchdog shape: the waiting code signals `main` (a promise) right before
//! the wait, then `main` prints and ends the process
//! (`process_exit(0)`). Had the wait returned, the waiting code's next line
//! would come first.

use crate::glue::{eprintln, process_exit};
use crate::lean::*;
use lean_runtime::sched::{self, ref_keyed, CtxId, DrainScope, Gate, Step};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// Resolve the signal `p` (`p.resolve ()`): `main` waits for it.
fn signal(p: &Obj<Promise<()>>) {
    p.resolve(());
}

/// `main` waits for promise `p` (`IO.wait p.result?`).
fn wait_for(p: &Obj<Promise<()>>) {
    let _ = p.result_opt().get();
}

// ---------------------------------------------------------------------------
// A thunk over a Gate (leanrs's `ThunkCell`)

struct ThunkCell<T> {
    value: OnceCell<T>,
    f: Cell<Option<Box<dyn FnOnce() -> T>>>,
    gate: Gate,
}

/// `Thunk α`: the closure runs at most once, across every copy; a force on
/// another context meanwhile waits for its store (`Gate::step`), a force
/// inside its own closure hangs (LB-08).
struct Thunk<T>(Rc<ThunkCell<T>>);

impl<T> Clone for Thunk<T> {
    fn clone(&self) -> Self {
        Thunk(self.0.clone())
    }
}

impl<T: Clone + 'static> Thunk<T> {
    fn new(f: impl FnOnce() -> T + 'static) -> Thunk<T> {
        Thunk(Rc::new(ThunkCell {
            value: OnceCell::new(),
            f: Cell::new(Some(Box::new(f))),
            gate: Gate::new(),
        }))
    }

    fn get(&self) -> T {
        let c = &*self.0;
        let key = Rc::as_ptr(&self.0) as *const () as usize;
        loop {
            if let Some(v) = c.value.get() {
                return v.clone();
            }
            match c.gate.step(key) {
                Step::Again => {}
                Step::Run => {
                    let Some(f) = c.f.take() else { sched::hang() };
                    let v = f();
                    c.gate.finish(key, || {
                        let _ = c.value.set(v);
                    });
                }
            }
        }
    }
}

/// leanrs's `thunk_forced_on_two_contexts`, with two waiters: `main` forces
/// a thunk whose closure waits for a promise; two dedicated tasks force it
/// meanwhile and wait; a third resolves the promise. The closure runs once,
/// every force gets its value, and the waiters wake in the order they
/// began to wait (W1). Then D8's probe: a thunk whose closure spawns a task
/// that reads the thunk: 5 and 6.
pub fn w1_gate_forcers(_: &[String]) -> u32 {
    let runs = Rc::new(Cell::new(0));
    let go: Obj<Promise<u64>> = Obj::new(Promise::new());
    let (r2, go2) = (runs.clone(), go.clone());
    let t = Thunk::new(move || {
        r2.set(r2.get() + 1);
        go2.result_opt().get().expect("resolved")
    });
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let forcer = |name: &'static str| {
        let (t, log) = (t.clone(), log.clone());
        as_task(
            move || {
                let v = t.get();
                log.borrow_mut().push(format!("{name} got {v}"));
            },
            PRIO_DEDICATED,
        )
    };
    let a = forcer("waiter 1");
    let b = forcer("waiter 2");
    let go3 = go.clone();
    let c = as_task(move || go3.resolve(5), PRIO_DEDICATED);
    let v = t.get();
    log.borrow_mut().push(format!("main got {v}"));
    a.get();
    b.get();
    c.get();
    for l in log.borrow().iter() {
        eprintln(l);
    }
    eprintln(&format!("closure runs: {}", runs.get()));
    // D8's probe
    type Pair = (u64, Task<u64>);
    let me: Rc<RefCell<Option<Thunk<Pair>>>> = Rc::new(RefCell::new(None));
    let inner = me.clone();
    let p: Thunk<Pair> = Thunk::new(move || {
        let again = inner.borrow().clone().expect("the thunk");
        (5, Task::spawn(move || again.get().0 + 1, PRIO_DEFAULT))
    });
    *me.borrow_mut() = Some(p.clone());
    let (x, task) = p.get();
    eprintln(&format!("probe: {x} {}", task.get()));
    *me.borrow_mut() = None;
    0
}

/// leanrs's `thunk_reentrant_never_ends` in the watchdog shape: a
/// dedicated task forces a thunk inside its own closure, which hangs while
/// the other contexts go on (LB-08); `main` ends the process. With arg
/// `exit`, `main` returns instead: the final run waits for the hung
/// context, and the process never ends, as `hang()`'s does.
pub fn w1_gate_self_force(args: &[String]) -> u32 {
    let reached: Obj<Promise<()>> = Obj::new(Promise::new());
    let me: Rc<RefCell<Option<Thunk<u64>>>> = Rc::new(RefCell::new(None));
    let (inner, r2) = (me.clone(), reached.clone());
    let t: Thunk<u64> = Thunk::new(move || {
        let again = inner.borrow().clone().expect("the thunk");
        signal(&r2);
        let v = again.get();
        eprintln("the self force returned");
        v
    });
    *me.borrow_mut() = Some(t.clone());
    let _task = as_task(move || t.get(), PRIO_DEDICATED);
    wait_for(&reached);
    eprintln("main: the task hangs in its own force");
    if args.first().map(String::as_str) == Some("exit") {
        return 0;
    }
    process_exit(0)
}

thread_local! {
    /// A static of the program over a Gate (leanrs's `LocalLazy`): its
    /// value, its gate, its initializer's runs.
    static STATIC: (OnceCell<u64>, Gate, Cell<u32>) = const { (OnceCell::new(), Gate::new(), Cell::new(0)) };
    static STATIC_GO: RefCell<Option<Obj<Promise<u64>>>> = const { RefCell::new(None) };
}

/// The static's read: its key is its address, stable while the thread
/// lives.
fn read_static() -> u64 {
    STATIC.with(|(cell, gate, runs)| {
        let key = gate as *const Gate as usize;
        loop {
            if let Some(v) = cell.get() {
                return *v;
            }
            match gate.step(key) {
                Step::Again => {}
                Step::Run => {
                    runs.set(runs.get() + 1);
                    let go = STATIC_GO.with(|g| g.borrow().clone()).expect("the gate");
                    let v = go.result_opt().get().expect("resolved");
                    gate.finish(key, || {
                        let _ = cell.set(v);
                    });
                }
            }
        }
    })
}

/// leanrs's `local_lazy_read_while_init_suspended`: `main` reads a static
/// whose initializer waits for a promise; a task reads it meanwhile and
/// waits; another resolves the promise; one value, one run of the
/// initializer.
pub fn w1_static_two_readers(_: &[String]) -> u32 {
    let go: Obj<Promise<u64>> = Obj::new(Promise::new());
    STATIC_GO.with(|g| *g.borrow_mut() = Some(go.clone()));
    let reader = as_task(read_static, PRIO_DEDICATED);
    let resolver = as_task(move || go.resolve(9), PRIO_DEDICATED);
    let mine = read_static();
    let theirs = reader.get();
    resolver.get();
    let runs = STATIC.with(|s| s.2.get());
    eprintln(&format!(
        "main {mine}, reader {theirs}, initializer runs {runs}"
    ));
    STATIC_GO.with(|g| *g.borrow_mut() = None);
    0
}

// ---------------------------------------------------------------------------
// lean2rr's shapes: constants and `busy` thunks over the keyed functions

thread_local! {
    /// Constant slots (lean2rr's once-cells): a value or none.
    static SLOTS: RefCell<Vec<Option<u64>>> = const { RefCell::new(Vec::new()) };
    static INIT_RUNS: Cell<u32> = const { Cell::new(0) };
}

/// A slot's key: odd, so it never meets an object's address.
fn slot_key(slot: usize) -> usize {
    (slot << 1) | 1
}

fn slot_get(slot: usize) -> Option<u64> {
    SLOTS.with(|s| s.borrow().get(slot).copied().flatten())
}

/// lean2rr's `once::claim`: whether constant `slot` has its value; if not,
/// the caller computes and stores it, unless another context computes it
/// (then wait, and look again).
fn claim(slot: usize) -> bool {
    slot_get(slot).is_some() || {
        loop {
            if slot_get(slot).is_some() {
                break true;
            }
            if sched::step_keyed(slot_key(slot)) {
                break false;
            }
        }
    }
}

/// lean2rr's `once::set_raw`: the store, then whoever waits goes on.
fn slot_set(slot: usize, v: u64) {
    sched::before_publish();
    SLOTS.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() <= slot {
            s.resize(slot + 1, None);
        }
        s[slot] = Some(v);
    });
    sched::done_keyed(slot_key(slot));
}

/// A constant whose initializer is `init`.
fn constant(slot: usize, init: impl FnOnce() -> u64) -> u64 {
    if !claim(slot) {
        INIT_RUNS.with(|n| n.set(n.get() + 1));
        let v = init();
        slot_set(slot, v);
    }
    slot_get(slot).expect("set")
}

/// Two contexts read a constant whose initializer waits for a promise
/// (lean2rr's slots, odd keys): the second waits for the first's store; one
/// run. With arg `self`, the initializer reads its own constant on the
/// same context (in a dedicated task): it hangs (LB-08) while `main` goes
/// on.
pub fn w1_keyed_constant(args: &[String]) -> u32 {
    if args.first().map(String::as_str) == Some("self") {
        let reached: Obj<Promise<()>> = Obj::new(Promise::new());
        let r2 = reached.clone();
        let _t = as_task(
            move || {
                constant(1, || {
                    signal(&r2);
                    let v = constant(1, || 0);
                    eprintln("the initializer's own read returned");
                    v
                })
            },
            PRIO_DEDICATED,
        );
        wait_for(&reached);
        eprintln("main: the initializer hangs in its own read");
        process_exit(0)
    }
    let go: Obj<Promise<u64>> = Obj::new(Promise::new());
    let go2 = go.clone();
    let init = move || go2.result_opt().get().expect("resolved");
    let reader = as_task(move || constant(0, || 0), PRIO_DEDICATED);
    let resolver = as_task(move || go.resolve(11), PRIO_DEDICATED);
    let mine = constant(0, init);
    let theirs = reader.get();
    resolver.get();
    let runs = INIT_RUNS.with(Cell::get);
    eprintln(&format!(
        "main {mine}, reader {theirs}, initializer runs {runs}"
    ));
    0
}

/// A lean2rr-style thunk cell: `pending`, `busy` (the forcer is not
/// recorded) or `done`.
struct BusyThunk {
    state: Cell<u8>,
    value: Cell<u64>,
    f: Cell<Option<Box<dyn FnOnce() -> u64>>>,
}

const PENDING: u8 = 0;
const BUSY: u8 = 1;
const DONE: u8 = 2;

impl BusyThunk {
    fn new(f: impl FnOnce() -> u64 + 'static) -> Rc<BusyThunk> {
        Rc::new(BusyThunk {
            state: Cell::new(PENDING),
            value: Cell::new(0),
            f: Cell::new(Some(Box::new(f))),
        })
    }

    /// lean2rr's generated force: `busy` calls `l2r_thunk_wait_busy`
    /// (`wait_running_keyed`), the store calls `l2r_thunk_done`
    /// (`done_keyed`) after the cell's store (with its writers point).
    fn get(self: &Rc<Self>) -> u64 {
        let key = Rc::as_ptr(self) as usize;
        loop {
            match self.state.get() {
                DONE => return self.value.get(),
                BUSY => sched::wait_running_keyed(key),
                _ => {
                    self.state.set(BUSY);
                    let f = self.f.take().expect("the closure");
                    let v = f();
                    sched::before_publish();
                    self.value.set(v);
                    self.state.set(DONE);
                    sched::done_keyed(key);
                }
            }
        }
    }
}

thread_local! {
    static INIT_THUNK: RefCell<Option<Rc<BusyThunk>>> = const { RefCell::new(None) };
}

/// The initializers of `w1_busy_thunk`: with `W1_BEFORE=1`, before the
/// task manager runs, a `busy` thunk forced again by its own closure hangs
/// at once.
pub fn w1_busy_init() {
    if std::env::var_os("W1_BEFORE").is_none() {
        return;
    }
    let me: Rc<RefCell<Option<Rc<BusyThunk>>>> = Rc::new(RefCell::new(None));
    let inner = me.clone();
    let t = BusyThunk::new(move || {
        eprintln("before the task manager: the closure forces its thunk");
        inner.borrow().clone().expect("the thunk").get()
    });
    *me.borrow_mut() = Some(t.clone());
    INIT_THUNK.with(|i| *i.borrow_mut() = Some(t.clone()));
    let v = t.get();
    eprintln(&format!("the self force returned {v}"));
}

/// lean2rr's `busy` thunk: `main` forces it, its closure waits for a
/// promise; a dedicated task forces it, finds it `busy` and waits
/// (`wait_running_keyed`) until `done_keyed`; another resolves. With arg
/// `alone`, the closure forces its own thunk with no other live context: it
/// hangs at once (the output, then the run's timeout).
pub fn w1_busy_thunk(args: &[String]) -> u32 {
    if args.first().map(String::as_str) == Some("alone") {
        let me: Rc<RefCell<Option<Rc<BusyThunk>>>> = Rc::new(RefCell::new(None));
        let inner = me.clone();
        let t = BusyThunk::new(move || {
            eprintln("the closure forces its own thunk, alone");
            inner.borrow().clone().expect("the thunk").get()
        });
        *me.borrow_mut() = Some(t.clone());
        let v = t.get();
        eprintln(&format!("the self force returned {v}"));
        return 0;
    }
    let go: Obj<Promise<u64>> = Obj::new(Promise::new());
    let go2 = go.clone();
    let t = BusyThunk::new(move || go2.result_opt().get().expect("resolved"));
    let t2 = t.clone();
    let reader = as_task(move || t2.get(), PRIO_DEDICATED);
    let resolver = as_task(move || go.resolve(13), PRIO_DEDICATED);
    let mine = t.get();
    let theirs = reader.get();
    resolver.get();
    eprintln(&format!("main {mine}, reader {theirs}"));
    0
}

/// W3 through the `extern "C"` keyed functions: a wait inside a
/// no-suspend scope is a Rust panic there, which aborts (status 134, the
/// reason on stderr). Arg `busy`: `wait_running_keyed`; `step`: the
/// runner's own `step_keyed`; `ref`: `ref_keyed::wait` of a taken
/// reference.
pub fn w1_w3_keyed(args: &[String]) -> u32 {
    let what = args.first().map(String::as_str).unwrap_or("");
    match what {
        "busy" => {
            let _scope = sched::no_suspend();
            sched::wait_running_keyed(64);
        }
        "step" => {
            assert!(sched::step_keyed(3));
            let _scope = sched::no_suspend();
            sched::step_keyed(3);
        }
        "ref" => {
            ref_keyed::take(128);
            let _scope = sched::no_suspend();
            ref_keyed::wait(128);
        }
        _ => {}
    }
    eprintln("not reached");
    0
}

// ---------------------------------------------------------------------------
// The keyed ST.Ref (lean2rr's shape) and its frame rule

/// A reference record whose cell holds the value (lean2rr's: a
/// placeholder while taken, here `None`), keyed by its address.
struct KRef {
    cell: RefCell<Option<u64>>,
}

impl KRef {
    fn new(v: u64) -> Rc<KRef> {
        Rc::new(KRef {
            cell: RefCell::new(Some(v)),
        })
    }

    fn key(self: &Rc<Self>) -> usize {
        Rc::as_ptr(self) as usize
    }

    fn get(self: &Rc<Self>) -> u64 {
        if ref_keyed::read_point() {
            ref_keyed::wait(self.key());
        }
        self.cell.borrow().expect("a full reference")
    }

    fn set(self: &Rc<Self>, v: u64) {
        if ref_keyed::write_point() {
            ref_keyed::store(self.key());
        }
        *self.cell.borrow_mut() = Some(v);
    }

    fn take(self: &Rc<Self>) -> u64 {
        ref_keyed::take(self.key());
        self.cell.borrow_mut().take().expect("a full reference")
    }

    /// `modify`: `take`, `f`, then the closing store, a `set` whose role
    /// the frame decides (lean2rr's option B).
    fn modify(self: &Rc<Self>, f: impl FnOnce(u64) -> u64) {
        let v = self.take();
        self.set(f(v));
    }
}

/// The frame rule with real contexts. Arg:
/// - `close`: a dedicated task's `modify` waits inside its function; `main`
///   and a reader task read meanwhile and wait; modify's own store (a `set`
///   in the taker's frame) closes the take and wakes them in the order they
///   began to wait; they read the stored value.
/// - `dep_store`: modify's function drops the last reference to a promise
///   whose `sync` dependent (a deeper frame) sets the reference: it waits,
///   as in 4.35 (forever: the taker waits for the dependent).
/// - `stack_task`: modify's function waits for a task that runs on the
///   taker's stack (a deeper frame) and sets the reference: it waits.
/// - `nested_take`: modify's function modifies the reference again: the
///   nested take waits (4.35).
/// - `own_get`: modify's function reads the reference (the taker's own
///   `get`, RS4-01): it waits.
pub fn w1_ref_keyed(args: &[String]) -> u32 {
    let what = args.first().map(String::as_str).unwrap_or("");
    let r = KRef::new(1);
    if what == "close" {
        let go: Obj<Promise<u64>> = Obj::new(Promise::new());
        let taken: Obj<Promise<()>> = Obj::new(Promise::new());
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let (r2, go2, t2) = (r.clone(), go.clone(), taken.clone());
        let m = as_task(
            move || {
                r2.modify(|v| {
                    signal(&t2);
                    v + go2.result_opt().get().expect("resolved")
                })
            },
            PRIO_DEDICATED,
        );
        wait_for(&taken);
        let (r3, log3) = (r.clone(), log.clone());
        let reader = as_task(
            move || {
                let v = r3.get();
                log3.borrow_mut().push(format!("reader got {v}"));
            },
            PRIO_DEDICATED,
        );
        // the reader starts, and waits, before `main` reads
        let started: Obj<Promise<()>> = Obj::new(Promise::new());
        let s2 = started.clone();
        let _s = as_task(move || signal(&s2), PRIO_DEDICATED);
        wait_for(&started);
        let go3 = go.clone();
        let _resolver = as_task(move || go3.resolve(41), PRIO_DEDICATED);
        let v = r.get();
        log.borrow_mut().push(format!("main got {v}"));
        m.get();
        reader.get();
        for l in log.borrow().iter() {
            eprintln(l);
        }
        eprintln(&format!("after modify: {}", r.get()));
        return 0;
    }
    let reached: Obj<Promise<()>> = Obj::new(Promise::new());
    let r2 = r.clone();
    let reached2 = reached.clone();
    let body: Box<dyn FnOnce(u64) -> u64> = match what {
        "dep_store" => {
            let p: Promise<()> = Promise::new();
            let (r4, re4) = (r.clone(), reached.clone());
            let _d = map_task(
                move |_| {
                    signal(&re4);
                    r4.set(100);
                    eprintln("the dependent's store returned");
                },
                p.result_opt(),
                PRIO_DEFAULT,
                true,
                true,
            );
            Box::new(move |v| {
                drop(p);
                v + 1
            })
        }
        "stack_task" => Box::new(move |v| {
            let (r4, re4) = (r2.clone(), reached2.clone());
            let q = Task::spawn(
                move || {
                    signal(&re4);
                    r4.set(100);
                    eprintln("the stack task's store returned");
                },
                PRIO_DEFAULT,
            );
            q.get();
            v + 1
        }),
        "nested_take" => Box::new(move |v| {
            signal(&reached2);
            r2.modify(|w| w + 1);
            eprintln("the nested modify returned");
            v + 1
        }),
        "own_get" => Box::new(move |v| {
            signal(&reached2);
            let w = r2.get();
            eprintln(&format!("the taker's own get returned {w}"));
            v + 1
        }),
        _ => return 2,
    };
    let r5 = r.clone();
    let _m = as_task(
        move || {
            r5.modify(body);
            eprintln("modify returned");
        },
        PRIO_DEDICATED,
    );
    wait_for(&reached);
    eprintln(&format!("main: {what} waits"));
    process_exit(0)
}

/// A promise dropped in a free (an `Arr`'s drain) has a `sync` dependent
/// that reaches a wait core: it reads a reference a task's `modify` holds,
/// and waits for its store. It runs after the drain, outside the
/// no-suspend scope (R2, R3), so the wait is a wait, not W3's panic: the
/// case of lean2rr's L6 (W3 is unreachable from Lean code).
pub fn w1_dependent_waits(_: &[String]) -> u32 {
    let r: Obj<sched::Ref<u64>> = Obj::new(sched::Ref::new(1));
    let taken: Obj<Promise<()>> = Obj::new(Promise::new());
    let go: Obj<Promise<u64>> = Obj::new(Promise::new());
    let (r2, t2, go2) = (r.clone(), taken.clone(), go.clone());
    let m = as_task(
        move || {
            r2.modify(|v| {
                signal(&t2);
                v + go2.result_opt().get().expect("resolved")
            })
        },
        PRIO_DEDICATED,
    );
    wait_for(&taken);
    let p: Promise<()> = Promise::new();
    let r3 = r.clone();
    let _d = map_task(
        move |_| {
            assert!(!DrainScope::active() && !sched::in_no_suspend());
            let v = r3.get();
            eprintln(&format!("dependent got {v}"));
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    let go3 = go.clone();
    let _resolver = as_task(move || go3.resolve(1), PRIO_DEDICATED);
    drop(Arr::new(vec![p]));
    eprintln("main: after the free");
    m.get();
    eprintln(&format!("after modify: {}", r.get()));
    0
}

/// A deferred resolution whose dependent hangs: the rest of its walk never
/// runs (R5), and stays pending (`deferred_pending`, the in-flight count);
/// the thread's list was moved out, so a later drain on another context
/// resolves in full. No entry is queued at any switch (R6, checked in debug
/// builds).
pub fn w1_drain_hang(_: &[String]) -> u32 {
    let reached: Obj<Promise<()>> = Obj::new(Promise::new());
    let r2 = reached.clone();
    let _a = as_task(
        move || {
            let px: Promise<()> = Promise::new();
            let py: Promise<()> = Promise::new();
            let _dx = map_task(
                move |_| {
                    signal(&r2);
                    sched::hang()
                },
                px.result_opt(),
                PRIO_DEFAULT,
                true,
                true,
            );
            let _dy = map_task(|_| eprintln("Y"), py.result_opt(), PRIO_DEFAULT, true, true);
            // freed from the last element: px first
            drop(Arr::new(vec![py, px]));
            eprintln("the hung walk went on");
        },
        PRIO_DEDICATED,
    );
    wait_for(&reached);
    eprintln(&format!("main: pending {}", sched::deferred_pending()));
    let pz: Promise<()> = Promise::new();
    let _dz = map_task(|_| eprintln("Z"), pz.result_opt(), PRIO_DEFAULT, true, true);
    drop(Arr::new(vec![pz]));
    eprintln(&format!("main: pending {}", sched::deferred_pending()));
    process_exit(0)
}

/// Review NEW-1 of wait-1: two contexts contend for one `flock` (leanrs's
/// and lean2rr's `IO.FS.Handle.lock` go through the crate's cooperative
/// `flock`). `main` holds the lock; a dedicated task signals `main`, then
/// waits in `Handle::lock`, napping in `block_until` and looking again;
/// `main` unlocks. The unlock itself must end the waiter's wait, not the
/// end of its nap (up to 16 ms later): the waiter reports how its last wait
/// ended (`io::flock_last_wake`, a test hook that records whether the
/// unlock found the waiter able to run), so the check needs no clock. The
/// waiter naps whenever `main` runs (the signal's wake does not switch), so
/// the unlock always finds it waiting or already runnable.
pub fn w1_flock_handoff(_: &[String]) -> u32 {
    use lean_runtime::io::{FsMode, Handle};
    let a = Handle::open(b"w1-flock", FsMode::Write).unwrap();
    a.lock(true).unwrap();
    let started: Obj<Promise<()>> = Obj::new(Promise::new());
    let s2 = started.clone();
    let waiter = as_task(
        move || {
            let b = Handle::open(b"w1-flock", FsMode::Write).unwrap();
            signal(&s2);
            b.lock(true).unwrap();
            let how = lean_runtime::io::flock_last_wake();
            b.unlock().unwrap();
            how
        },
        PRIO_DEDICATED,
    );
    wait_for(&started);
    // the waiter naps in its `flock` now
    a.unlock().unwrap();
    let how = waiter.get();
    eprintln(&format!("the waiter's flock ended with: {how:?}"));
    0
}

// ---------------------------------------------------------------------------
// 3000 schedules of ST.Ref operations against Lean 4.35's store model

/// A value whose live copies are counted.
struct Val {
    v: u64,
    live: Rc<Cell<i64>>,
}

impl Val {
    fn new(v: u64, live: &Rc<Cell<i64>>) -> Val {
        live.set(live.get() + 1);
        Val {
            v,
            live: live.clone(),
        }
    }
}

impl Clone for Val {
    fn clone(&self) -> Self {
        Val::new(self.v, &self.live)
    }
}

impl Drop for Val {
    fn drop(&mut self) {
        self.live.set(self.live.get() - 1)
    }
}

/// The reference under test: the object form (`sched::Ref`) or the keyed
/// form (`ref_keyed` around a record's cell).
enum TRef {
    Object(sched::Ref<Val>),
    Keyed(RefCell<Option<Val>>),
}

impl TRef {
    fn key(&self) -> usize {
        self as *const TRef as usize
    }

    fn get(&self) -> Val {
        match self {
            TRef::Object(r) => r.get(),
            TRef::Keyed(c) => {
                if ref_keyed::read_point() {
                    ref_keyed::wait(self.key());
                }
                c.borrow().clone().expect("full")
            }
        }
    }

    fn take(&self) -> Val {
        match self {
            TRef::Object(r) => r.take(),
            TRef::Keyed(c) => {
                ref_keyed::take(self.key());
                c.borrow_mut().take().expect("full")
            }
        }
    }

    /// `swap`, or `set` (whose old value is dropped: `None`).
    fn store(&self, v: Val, swap: bool) -> Option<Val> {
        match self {
            TRef::Object(r) if swap => Some(r.swap(v)),
            TRef::Object(r) => {
                r.set(v);
                None
            }
            TRef::Keyed(c) => {
                let p = if swap {
                    ref_keyed::swap_point()
                } else {
                    ref_keyed::write_point()
                };
                if p {
                    ref_keyed::store(self.key());
                }
                let old = c.borrow_mut().replace(v).expect("full");
                swap.then_some(old)
            }
        }
    }

    /// `modify`'s closing store.
    fn put(&self, v: Val) {
        match self {
            TRef::Object(r) => r.put(v),
            TRef::Keyed(c) => {
                if ref_keyed::write_point() {
                    ref_keyed::store(self.key());
                }
                let old = c.borrow_mut().replace(v);
                assert!(old.is_none(), "a closing store into a full cell");
            }
        }
    }

    /// The value now, with no wait (`None` while taken): the keyed form
    /// only; the object form answers through `get` when the model says it
    /// is full.
    fn peek(&self) -> Option<u64> {
        match self {
            TRef::Object(r) => Some(r.get().v),
            TRef::Keyed(c) => c.borrow().as_ref().map(|v| v.v),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Get(usize),
    Set(usize, u64),
    Swap(usize, u64),
    /// take, `k` yields, then put of the taken value plus `d`
    Modify(usize, u64, u32),
    Yield,
    /// a modify of `r` whose function modifies `r` again: its inner take
    /// waits on its own empty cell (a deadlock, as in 4.35)
    NestedModify(usize),
}

impl Op {
    fn cell(self) -> Option<usize> {
        match self {
            Op::Get(r) | Op::Set(r, _) | Op::Swap(r, _) | Op::Modify(r, ..) => Some(r),
            Op::NestedModify(r) => Some(r),
            Op::Yield => None,
        }
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    /// at a yield point, until the schedule picks it
    Parked,
    /// running, or blocked inside an operation
    Busy,
    Done,
}

/// The shared state of one schedule: the model and what each context does.
struct World {
    seed: u64,
    model: Vec<Option<u64>>,
    /// every completed store per cell, in order
    hist: Vec<Vec<u64>>,
    st: Vec<St>,
    ctx: Vec<Option<CtxId>>,
    /// the operation each context is in
    op: Vec<Option<Op>>,
    /// whether a context holds a value it took (a nested modify's)
    holds: Vec<bool>,
    /// a pending store's value is alive in the context's frame
    pending: Vec<bool>,
    /// what each context read, (cell, value)
    log: Vec<(usize, u64)>,
    activity: u64,
}

type W = Rc<RefCell<World>>;

/// A yield point of context `c`: parked until the schedule picks it.
fn park(w: &W, c: usize) {
    {
        let mut x = w.borrow_mut();
        x.st[c] = St::Parked;
        x.ctx[c] = Some(sched::current_context());
        x.activity += 1;
    }
    sched::block_sync();
}

fn step_done(w: &W) {
    w.borrow_mut().activity += 1;
}

/// Context `c`'s script.
fn run_ops(w: W, refs: Rc<Vec<TRef>>, live: Rc<Cell<i64>>, c: usize, ops: Vec<Op>) {
    for op in ops {
        park(&w, c);
        w.borrow_mut().op[c] = Some(op);
        match op {
            Op::Yield => {}
            Op::Get(r) => {
                let v = refs[r].get();
                let mut x = w.borrow_mut();
                let seed = x.seed;
                assert_eq!(Some(v.v), x.model[r], "seed {seed}: get");
                x.log.push((r, v.v));
            }
            Op::Set(r, k) | Op::Swap(r, k) => {
                let swap = matches!(op, Op::Swap(..));
                w.borrow_mut().pending[c] = true;
                let old = refs[r].store(Val::new(k, &live), swap);
                let mut x = w.borrow_mut();
                let seed = x.seed;
                x.pending[c] = false;
                assert!(
                    x.model[r].is_some(),
                    "seed {seed}: a store completed on an empty cell (a lost update)"
                );
                if let Some(old) = old {
                    assert_eq!(Some(old.v), x.model[r], "seed {seed}: swap's old value");
                    assert_eq!(
                        x.hist[r].last(),
                        Some(&old.v),
                        "seed {seed}: a swap displaced a value other than the last store"
                    );
                    x.log.push((r, old.v));
                }
                x.model[r] = Some(k);
                x.hist[r].push(k);
            }
            Op::Modify(r, d, k) => {
                let v = refs[r].take();
                let held_len = {
                    let mut x = w.borrow_mut();
                    let seed = x.seed;
                    assert_eq!(Some(v.v), x.model[r], "seed {seed}: modify's take");
                    assert_eq!(
                        x.hist[r].last(),
                        Some(&v.v),
                        "seed {seed}: modify took a stale value"
                    );
                    x.model[r] = None;
                    x.hist[r].len()
                };
                for _ in 0..k {
                    // a yield inside modify's function
                    park(&w, c);
                }
                {
                    let mut x = w.borrow_mut();
                    let seed = x.seed;
                    assert!(
                        x.model[r].is_none(),
                        "seed {seed}: the cell was filled while modify held it"
                    );
                    assert_eq!(
                        x.hist[r].len(),
                        held_len,
                        "seed {seed}: a store landed while modify held the cell (a lost update)"
                    );
                    x.model[r] = Some(v.v + d);
                    x.hist[r].push(v.v + d);
                }
                refs[r].put(Val::new(v.v + d, &live));
            }
            Op::NestedModify(r) => {
                let v = refs[r].take();
                {
                    let mut x = w.borrow_mut();
                    let seed = x.seed;
                    assert_eq!(Some(v.v), x.model[r], "seed {seed}: the outer take");
                    x.model[r] = None;
                    x.holds[c] = true;
                }
                park(&w, c);
                let _inner = refs[r].take();
                panic!("seed {}: the nested take returned", w.borrow().seed);
            }
        }
        w.borrow_mut().op[c] = None;
        step_done(&w);
    }
    let mut x = w.borrow_mut();
    x.st[c] = St::Done;
    x.activity += 1;
}

/// Let every context that can run, run, until none can (the schedule's
/// step is over).
fn settle(w: &W) {
    loop {
        let a = w.borrow().activity;
        sched::poll();
        if w.borrow().activity == a {
            return;
        }
    }
}

/// One schedule: the number of operations it saw blocked (after a step),
/// and whether it ended in a deadlock.
fn one_schedule(seed: u64, keyed: bool) -> (u64, bool) {
    let live = Rc::new(Cell::new(0i64));
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let nrefs = 2usize;
    let refs: Rc<Vec<TRef>> = Rc::new(
        (0..nrefs)
            .map(|i| {
                let v = Val::new(i as u64 * 1000, &live);
                if keyed {
                    TRef::Keyed(RefCell::new(Some(v)))
                } else {
                    TRef::Object(sched::Ref::new(v))
                }
            })
            .collect(),
    );
    let nctx = 2 + rng.below(3) as usize;
    let nested = seed.is_multiple_of(50);
    let scripts: Vec<Vec<Op>> = (0..nctx)
        .map(|c| {
            if nested && c == 0 {
                return vec![Op::NestedModify(0)];
            }
            let n = 2 + rng.below(6);
            (0..n)
                .map(|_| match rng.below(6) {
                    0 => Op::Get(rng.below(2) as usize),
                    1 => Op::Set(rng.below(2) as usize, 1 + rng.below(100)),
                    2 => Op::Swap(rng.below(2) as usize, 1 + rng.below(100)),
                    3 | 4 => {
                        Op::Modify(rng.below(2) as usize, 1 + rng.below(9), rng.below(4) as u32)
                    }
                    _ => Op::Yield,
                })
                .collect()
        })
        .collect();
    let w: W = Rc::new(RefCell::new(World {
        seed,
        model: (0..nrefs).map(|i| Some(i as u64 * 1000)).collect(),
        hist: (0..nrefs).map(|i| vec![i as u64 * 1000]).collect(),
        st: vec![St::Busy; nctx],
        ctx: vec![None; nctx],
        op: vec![None; nctx],
        holds: vec![false; nctx],
        pending: vec![false; nctx],
        log: Vec::new(),
        activity: 0,
    }));
    let mut tasks = Vec::new();
    for (c, ops) in scripts.into_iter().enumerate() {
        let (w2, refs2, live2) = (w.clone(), refs.clone(), live.clone());
        tasks.push(as_task(
            move || run_ops(w2, refs2, live2, c, ops),
            PRIO_DEDICATED,
        ));
    }
    settle(&w);
    let mut steps = 0u64;
    let mut blocked = 0u64;
    let deadlock = loop {
        // every context blocked inside an operation waits on a cell the
        // model says is empty
        {
            let x = w.borrow();
            for c in 0..nctx {
                if x.st[c] == St::Busy {
                    let r = x.op[c].and_then(Op::cell).expect("blocked in an operation");
                    assert!(
                        x.model[r].is_none(),
                        "seed {seed}: a wait the model does not have ({:?})",
                        x.op[c]
                    );
                    blocked += 1;
                }
            }
        }
        let parked: Vec<usize> = (0..nctx)
            .filter(|&c| w.borrow().st[c] == St::Parked)
            .collect();
        if parked.is_empty() {
            break w.borrow().st.iter().any(|s| *s != St::Done);
        }
        let c = parked[rng.below(parked.len() as u64) as usize];
        steps += 1;
        let ctx = {
            let mut x = w.borrow_mut();
            x.st[c] = St::Busy;
            x.ctx[c].expect("parked")
        };
        sched::wake(ctx);
        settle(&w);
        assert!(steps < 100_000, "seed {seed}: no progress");
    };
    {
        let x = w.borrow();
        for &(r, v) in &x.log {
            assert!(
                x.hist[r].contains(&v),
                "seed {seed}: read {v} was never stored"
            );
        }
        for r in 0..nrefs {
            if let Some(v) = x.model[r] {
                assert_eq!(
                    x.hist[r].last(),
                    Some(&v),
                    "seed {seed}: the last store was lost"
                );
            }
        }
    }
    let model = w.borrow().model.clone();
    for r in 0..nrefs {
        match model[r] {
            Some(v) => assert_eq!(refs[r].peek(), Some(v), "seed {seed}: final contents"),
            None if keyed => assert_eq!(refs[r].peek(), None, "seed {seed}: final contents"),
            None => {}
        }
    }
    if deadlock {
        // the contexts that wait forever keep what their frames hold
        let x = w.borrow();
        let held = (0..nctx)
            .filter(|&c| x.st[c] != St::Done && (x.holds[c] || x.pending[c]))
            .count() as i64;
        let full = model.iter().filter(|m| m.is_some()).count() as i64;
        assert_eq!(
            live.get(),
            held + full,
            "seed {seed}: values alive in a deadlock"
        );
    } else {
        for t in tasks {
            t.get();
        }
        drop(refs);
        assert_eq!(
            live.get(),
            0,
            "seed {seed}: a value not dropped exactly once"
        );
    }
    let _ = steps;
    (blocked, deadlock)
}

/// leanrs's evidence `beh_ref_empty_cell.rs` on the real types (R1 of the
/// proof review): the contexts are dedicated tasks running scripts of
/// reference operations; the schedule (a seeded choice of which parked
/// context goes next, the yield points being the steps) is driven by
/// `main`; a wait is the real one (`block_sync`), and the contexts it wakes
/// run in the scheduler's order. Each step is checked against Lean 4.35's
/// store model: the same results, the same waits, the same deadlocks, no
/// `RefCell` panic, every value dropped exactly once (or kept by a context
/// that waits forever), no lost update. Arg `object`: `sched::Ref`;
/// `keyed`: `ref_keyed` around a cell. The deadlocked contexts never end,
/// so the program ends with `process_exit`.
pub fn w1_ref_schedules(args: &[String]) -> u32 {
    let keyed = args.first().map(String::as_str) == Some("keyed");
    let seeds = args.get(1).map(|s| to_nat(s)).unwrap_or(3000);
    // The model's steps are the scripts' yield points: the reads' polling
    // points (every 1000th read) would let a context yield in the middle of
    // an operation, which `main`'s schedule would take for a wait.
    sched::set_ref_read_yields(false);
    let (mut blocked, mut deadlocks) = (0u64, 0u64);
    for seed in 1..=seeds {
        let (b, d) = one_schedule(seed, keyed);
        blocked += b;
        deadlocks += d as u64;
    }
    assert!(deadlocks >= seeds / 50, "the nested takes deadlock");
    assert!(
        blocked >= seeds / 2,
        "coverage: {blocked} blocked operations"
    );
    eprintln(&format!(
        "{} form: {seeds} schedules agree with Lean 4.35's store model",
        if keyed { "keyed" } else { "object" }
    ));
    eprintln(&format!(
        "({blocked} operations seen waiting, {deadlocks} deadlocks)"
    ));
    process_exit(0)
}
