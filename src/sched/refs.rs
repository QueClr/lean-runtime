//! Core 3.2 of the wait cores (batch wait-1, `docs/sched.md`, "The wait
//! cores"): `IO.Ref` (`ST.Ref`) under Lean 4.35's rule on the single-thread
//! scheduler, as an object (`Ref<T>`, for a translator whose reference can
//! hold it: leanrs, the crate's driver) and as keyed functions
//! (`ref_keyed`, for a translator whose reference record cannot hold a
//! waiter list: lean2rr). Threads mode's `sched::Ref` is `mt::Ref`, with
//! the same public API and the same rule.
//!
//! The rule (LB-01 and LB-18 of `docs/lean-bugs.md`; `docs/threads.md`,
//! 3.1):
//! - `modify` is `take`, then `put` (`ST.Prim.Ref.modifyUnsafe`): the
//!   reference is empty while its function runs, and that function may
//!   block (a `Task.get`), letting other contexts run;
//! - only `modify`'s closing store (`put`) fills the empty reference and
//!   wakes the contexts waiting for it, in the order they began to wait;
//! - until then `get`, `take`, `set` and `swap` wait: each is a blocking
//!   yield point (`block_sync`), and the context keeps its worker, as a
//!   native thread spinning on the empty slot keeps its own (`io.cpp`
//!   1459-1500). This includes the taker's own `get` and `take`, and a
//!   store from code nested inside `modify`'s function (a `sync` dependent
//!   of a promise the function drops): they wait forever, as natively the
//!   taker's own `get` spins forever (review RS4-01; Lean 4.35 deadlocks
//!   the same way);
//! - `set` is `swap` with the result dropped (4.35's `Ref.set`), so a `set`
//!   is never lost (LB-01); `swap` returns the value the reference held,
//!   never its own argument (LB-18);
//! - so `modify` and `swap` are atomic. The cost, as in 4.35: a `modify`
//!   whose function waits for a task that uses the same reference
//!   deadlocks.
//!
//! The yield points: `get`, `take` and `swap` call `ref_read()` (a polling
//! point every `REF_READS_PER_POLL`-th read, once `set_ref_read_yields` is
//! on); `set`, `swap`, `take` and `put` call `before_publish()` (a writers
//! point). Both come before any borrow, since both may switch.
//!
//! No borrow is held across a wait, a drop of a translator value or
//! translator code (W2), with one exception: `T::clone` inside `get`'s
//! borrow, which must not use this reference. A value a store replaces is
//! dropped after the borrow, so its destructor may do anything.
//!
//! A Lean panic in `modify`'s function returns its default value, so the
//! closing store still runs. A Rust panic leaves the reference empty; the
//! glue ends the process rather than going on.

use super::wait::{before_block, wake_in_order, Table, WaitList};
use super::{alive, before_publish, block_sync, current_context, ref_read, with, CtxId, MAIN};
use std::cell::RefCell;

/// `ST.Ref α` (`lean_st_ref_*`) under Lean 4.35's rule, on the
/// single-thread scheduler (the rule of the module). A translator keeps it
/// in its own counted handle (leanrs: `Rc`; the crate's driver: `Rc`);
/// threads mode's `sched::Ref` (`mt::Ref`) has the same API.
///
/// Example: `let r = Ref::new(1); r.modify(|v| v + 1); assert_eq!(r.get(),
/// 2);`. While the function of a `modify` on another context blocks, a
/// `get` here waits for its store and returns its value.
pub struct Ref<T> {
    /// The content; `None` while a `take` holds the reference.
    val: RefCell<Option<T>>,
    /// The contexts waiting for the closing store.
    waiters: WaitList,
}

impl<T> Ref<T> {
    /// `ST.mkRef` (`lean_st_mk_ref`).
    pub fn new(v: T) -> Ref<T> {
        Ref {
            val: RefCell::new(Some(v)),
            waiters: WaitList::new(),
        }
    }

    /// A reference with no value, which well-typed code never reads (a
    /// translator's placeholder). An operation on it other than `put`
    /// waits forever.
    pub fn empty() -> Ref<T> {
        Ref {
            val: RefCell::new(None),
            waiters: WaitList::new(),
        }
    }

    /// Wait for the closing store of the empty reference: register the
    /// running context with no borrow of the content held, then block
    /// until `put` wakes it (`WaitList::wait`; a Rust panic inside a
    /// no-suspend scope, W3).
    #[cold]
    #[inline(never)]
    fn wait_full(&self) {
        self.waiters.wait();
    }

    /// `ST.Ref.get` (`lean_st_ref_get`): a clone of the value, made inside
    /// the borrow, once the reference holds one. Never puts anything back.
    #[inline]
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        ref_read();
        loop {
            if let Some(v) = &*self.val.borrow() {
                return v.clone();
            }
            self.wait_full();
        }
    }

    /// `ST.Ref.take` (`lean_st_ref_take`): the value moved out, once the
    /// reference holds one; the reference stays empty until `put`.
    #[inline]
    pub fn take(&self) -> T {
        before_publish();
        ref_read();
        loop {
            let v = self.val.borrow_mut().take();
            if let Some(v) = v {
                return v;
            }
            self.wait_full();
        }
    }

    /// The exchange of `swap` and `set` once the reference is full: `a`
    /// moved in, the old value out, returned after the borrow ends.
    fn exchange(&self, a: T) -> T {
        let mut a = Some(a);
        loop {
            {
                let mut cell = self.val.borrow_mut();
                if cell.is_some() {
                    let old = std::mem::replace(&mut *cell, a.take());
                    drop(cell);
                    match old {
                        Some(old) => return old,
                        None => unreachable!("lean-runtime: Ref::exchange"),
                    }
                }
            }
            self.wait_full();
        }
    }

    /// Lean 4.35's `ST.Ref.put` (`lean_st_ref_put`), `modify`'s closing
    /// store: fill the reference that `take` emptied, then wake whoever
    /// waits for it, in the order they began to wait. A put into a full
    /// reference (a glue's error; 4.35 asserts) is a debug assertion, and
    /// replaces the value otherwise.
    #[inline]
    pub fn put(&self, v: T) {
        before_publish();
        let old = self.val.borrow_mut().replace(v);
        debug_assert!(old.is_none(), "lean-runtime: put into a full reference");
        drop(old);
        self.waiters.wake_all();
    }

    /// `ST.Ref.swap` (`lean_st_ref_swap`): the value the reference held,
    /// once it holds one, and `v` in its place.
    #[inline]
    pub fn swap(&self, v: T) -> T {
        before_publish();
        ref_read();
        self.exchange(v)
    }

    /// `ST.Ref.set` (`lean_st_ref_set`; 4.35: `discard <| Ref.swap r a`):
    /// waits while the reference is empty, so it is never lost. The old
    /// value is dropped after the borrow.
    #[inline]
    pub fn set(&self, v: T) {
        before_publish();
        drop(self.exchange(v));
    }

    /// `ST.Ref.modify` (`Ref.modifyUnsafe`): `take`, `f` with no borrow
    /// held, then `put`.
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        let v = self.take();
        self.put(f(v));
    }

    /// `ST.Ref.modifyGet` (`Ref.modifyGetUnsafe`): `take`, `f` with no
    /// borrow held, then `put` of its second component; its first is
    /// returned.
    pub fn modify_get<R>(&self, f: impl FnOnce(T) -> (R, T)) -> R {
        let v = self.take();
        let (r, v) = f(v);
        self.put(v);
        r
    }
}

/// The running frame: the running context, the number of tasks running on
/// it, and the innermost one's entry and generation. `modify`'s `take` and
/// its closing store run in one frame; everything the scheduler runs nested
/// inside `modify`'s function (a task run on the waiting context's stack, a
/// `sync` dependent of a promise resolved or dropped there) passes through
/// `run_task`, which begins it at a deeper frame. Thunks and constants
/// forced in the function are pure code: they reach a reference only
/// through such nested tasks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Frame {
    ctx: CtxId,
    depth: u32,
    top: Option<(u32, u32)>,
}

impl Frame {
    /// `main`'s frame outside any task.
    const OUTSIDE: Frame = Frame {
        ctx: MAIN,
        depth: 0,
        top: None,
    };
}

/// The running frame. Before the task manager runs there is one context,
/// `main`'s, and no task runs through the scheduler (they run at once), so
/// the frame is `main`'s outside any task, found without building the
/// scheduler's state (review RS4-02); also once the thread's locals are
/// gone. Otherwise one borrow of the scheduler's state.
pub(crate) fn frame() -> Frame {
    if !super::manager_running() || !alive() {
        return Frame::OUTSIDE;
    }
    let (ctx, depth, top) = with(|s| s.frame());
    Frame { ctx, depth, top }
}

/// The keyed form of the rule (single-thread only), for a translator whose
/// reference is a record that cannot hold a waiter list (lean2rr): the
/// taken references and their waiters are in this thread's table, under
/// the record's address (W5; even: debug builds check it). The translator
/// keeps the value in its record, and calls these around its own cell
/// operations, which come right after the call (no switch in between):
/// - `get`: `if read_point() { wait(key) }`, then the cell's read;
/// - `set`: `if write_point() { store(key) }`, then the cell's store;
/// - `swap`: `if swap_point() { store(key) }`, then the cell's exchange;
/// - `take`: `take(key)`, then the cell's move (it holds a placeholder
///   until the closing store);
/// - `modify`'s closing store: `if write_point() { store(key) }` (the
///   frame decides, the translator's option B), or `write_point();
///   put(key)` where the translator knows the store statically (option A),
///   then the cell's store.
///
/// `store` closes the take only in the frame that took the reference
/// (`Frame`): `modify`'s own store, in safe code. A store from anywhere
/// else, a `sync` dependent nested inside `modify`'s function included,
/// waits, as in Lean 4.35. In `unsafe` code, a second store in the taker's
/// own frame would close the take early, where a static analysis would
/// mark the same store.
pub mod ref_keyed {
    use super::*;
    use std::cell::Cell;

    /// A reference `take` emptied: its key, the frame that took it, and the
    /// contexts waiting for its closing store, in the order they began to
    /// wait.
    struct Taken {
        key: usize,
        taker: Frame,
        waiters: Vec<CtxId>,
    }

    thread_local! {
        /// The references taken now (usually none or one: a `modify` whose
        /// function runs). Its first 4 entries need no allocation (AR-40).
        static TAKEN: RefCell<Table<Taken, 4>> = const { RefCell::new(Table::new()) };
        /// `TAKEN`'s length, for the points' inline test.
        static TAKEN_LEN: Cell<u32> = const { Cell::new(0) };
    }

    fn set_len(t: &Table<Taken, 4>) {
        let _ = TAKEN_LEN.try_with(|n| n.set(t.len() as u32));
    }

    /// Whether a reference is taken on this thread: one thread-local load.
    #[inline]
    fn any_taken() -> bool {
        TAKEN_LEN.try_with(Cell::get).unwrap_or(0) != 0
    }

    /// Before a `get`: `ref_read()`, then whether some reference is taken
    /// (then the translator calls `wait`). One thread-local load past the
    /// point (W4).
    #[inline]
    pub fn read_point() -> bool {
        ref_read();
        any_taken()
    }

    /// Before a `set`: `before_publish()`, then the same test (then the
    /// translator calls `store`).
    #[inline]
    pub fn write_point() -> bool {
        before_publish();
        any_taken()
    }

    /// Before a `swap`: both points, then the same test (then `store`).
    #[inline]
    pub fn swap_point() -> bool {
        before_publish();
        ref_read();
        any_taken()
    }

    /// The frame that took `key`, if it is taken.
    fn taker(key: usize) -> Option<Frame> {
        TAKEN
            .try_with(|t| t.borrow().find(|e| e.key == key).map(|e| e.taker))
            .ok()
            .flatten()
    }

    /// Register the running context as a waiter for `key`'s closing store
    /// and block until it (W3: a Rust panic inside a no-suspend scope, an
    /// abort in these `extern "C"` functions; W6: a hang once the thread's
    /// locals are gone).
    fn wait_taken(key: usize) {
        before_block();
        let me = current_context();
        let registered = TAKEN
            .try_with(|t| {
                let mut t = t.borrow_mut();
                match t.find_mut(|e| e.key == key) {
                    Some(e) => {
                        e.waiters.push(me);
                        true
                    }
                    None => false,
                }
            })
            .unwrap_or(false);
        if registered {
            block_sync();
        }
    }

    /// `key`'s take is closed: its entry goes, its waiters wake in order
    /// (none: no scheduler state is touched, `wake_in_order`). Never
    /// switches.
    fn untake(key: usize) -> bool {
        let ws = TAKEN
            .try_with(|t| {
                let mut t = t.borrow_mut();
                let e = t.remove(|e| e.key == key)?;
                set_len(&t);
                Some(e.waiters)
            })
            .ok()
            .flatten();
        let closed = ws.is_some();
        if let Some(ws) = ws {
            wake_in_order(ws);
        }
        closed
    }

    /// `take` of the reference whose record is at `key`, before the cell's
    /// move: `before_publish()` and `ref_read()`, then, while the reference
    /// is taken (by anyone, the running frame included: a nested take waits,
    /// as in 4.35), wait for its closing store; then it is taken by the
    /// running frame.
    #[cold]
    #[inline(never)]
    pub extern "C" fn take(key: usize) {
        debug_assert!(
            key & 1 == 0,
            "lean-runtime: a reference's key is its address (even)"
        );
        before_publish();
        ref_read();
        let f = frame();
        loop {
            match taker(key) {
                Some(_) => wait_taken(key),
                None => {
                    let _ = TAKEN.try_with(|t| {
                        let mut t = t.borrow_mut();
                        t.push(Taken {
                            key,
                            taker: f,
                            waiters: Vec::new(),
                        });
                        set_len(&t);
                    });
                    return;
                }
            }
        }
    }

    /// A `get`, or a store that is not the closing store, of the reference
    /// at `key`, while some reference is taken (`read_point`): while `key`
    /// is taken, by anyone (the taker's own `get` included, review RS4-01),
    /// wait for its closing store. Returns once it is not taken.
    #[cold]
    #[inline(never)]
    pub extern "C" fn wait(key: usize) {
        debug_assert!(
            key & 1 == 0,
            "lean-runtime: a reference's key is its address (even)"
        );
        while taker(key).is_some() {
            wait_taken(key);
        }
    }

    /// `modify`'s closing store, where the translator knows it statically:
    /// the reference at `key` is no longer taken, and its waiters wake in
    /// order. It makes no writers point: call `write_point()` first, as
    /// before every store (review RW1-07). A put of a reference that is not
    /// taken (a glue's error) is a debug assertion, and does nothing
    /// otherwise.
    #[cold]
    #[inline(never)]
    pub extern "C" fn put(key: usize) {
        debug_assert!(
            key & 1 == 0,
            "lean-runtime: a reference's key is its address (even)"
        );
        let closed = untake(key);
        debug_assert!(
            closed,
            "lean-runtime: a closing store of a reference not taken"
        );
    }

    /// A `set` or `swap` of the reference at `key` whose role is decided at
    /// run time, while some reference is taken (`write_point`,
    /// `swap_point`): if the running frame took `key`, it is the closing
    /// store (`put`); otherwise, while `key` is taken, wait for its closing
    /// store, then return (the plain store follows).
    #[cold]
    #[inline(never)]
    pub extern "C" fn store(key: usize) {
        debug_assert!(
            key & 1 == 0,
            "lean-runtime: a reference's key is its address (even)"
        );
        let f = frame();
        loop {
            match taker(key) {
                None => return,
                Some(t) if t == f => {
                    untake(key);
                    return;
                }
                Some(_) => wait_taken(key),
            }
        }
    }

    /// Whether `key` is taken (unit tests).
    #[cfg(test)]
    pub(crate) fn is_taken(key: usize) -> bool {
        TAKEN.with(|t| t.borrow().find(|e| e.key == key).is_some())
    }
}
