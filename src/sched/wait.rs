//! Core 3.1 of the wait cores (batch wait-1, `docs/sched.md`, "The wait
//! cores"): a context waits for a computation that another context runs,
//! as a native thread waits for a thunk another thread forces
//! (`lean_thunk_get_core`, `object.cpp` 540-565) or for a constant another
//! thread initializes (`lean_obj_once_cold`, 2880-2904).
//!
//! A computation has a value, or it does not. Without one it may have a
//! *runner*: the context that claimed it and runs it. The rules:
//! - no value, no runner: the running context becomes the runner, computes
//!   the value, and stores it through the core's finish (`Gate::finish`,
//!   `done_keyed`), which wakes the waiters;
//! - no value, the running context is the runner (the computation needs
//!   itself): `hang()`, while the other contexts go on (LB-08 of
//!   `docs/lean-bugs.md`: natively the thread spins forever);
//! - no value, another context is the runner: the running context waits
//!   (`block_sync`, a blocking yield point that keeps its worker, as a
//!   spinning native thread keeps its own), and looks again when woken;
//! - the waiters wake in the order they began to wait (W1), and a wake
//!   never switches.
//!
//! The waiters of a computation are kept in this thread's keyed table under
//! the computation's key, so a translator's cell holds at most the 4-byte
//! `Gate` (the runner), or nothing at all (the keyed functions, for cells
//! with no room: lean2rr's).
//!
//! Single-thread only: `CtxId`, `block_sync` and `wake` are the
//! single-thread scheduler's. In threads mode (the crate's own drivers so
//! far: neither translator has a threads build) a thunk or a constant
//! forced on two threads blocks the OS thread through the glue's
//! `OnceLock` or lock (`docs/threads.md`).
//!
//! **Keys** (W5). A key is a `usize` that names one computation while it
//! has an entry; the crate never dereferences it. The runner and every
//! waiter hold a reference to the object, so its address names one live
//! object while it has an entry. Object keys are addresses, which are even
//! (debug builds check it); a translator that keys an index (lean2rr's
//! constant slots) passes `(index << 1) | 1`, so the two spaces never meet.
//! One object uses either a `Gate` or the keyed functions, never both.
//!
//! **A Rust panic out of the computation** leaves the runner recorded:
//! nothing clears it while the panic unwinds. A later step on the same
//! context then hangs, and on another context waits forever. Both
//! translators end the process on a Rust panic (`panic = "abort"`), so this
//! is not a case.

use super::ctx::{CtxId, MAIN};
use super::{alive, before_publish, block_sync, current_context, hang, hang_thread, wake, with};
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;

/// W3's message: a wait core would block inside a no-suspend scope.
pub const WAIT_IN_NO_SUSPEND: &str = "lean-runtime: a wait inside a no-suspend scope (a free)";

/// The record of "no runner" (`CtxId::index` is never `u32::MAX`).
const NO_RUNNER: u32 = u32::MAX;

/// Right before a core blocks (a wait or a hang). W6 first: once the
/// thread's locals are gone (a translator's thread-local destructed at
/// exit), the thread waits forever, as nothing can wake it. Then W3: inside
/// a no-suspend scope (a translator's free or drop path, which must not
/// suspend: lean2rr's Reussir drain is the thread's, leanrs's drain must not
/// switch), a Rust panic with the reason, never a suspension. Both
/// translators enter the scope only for frees, and a free runs no Lean code
/// (the promises it drops are resolved after it, `sched::defer`), so no
/// Lean program reaches the panic (`docs/sched.md`, "W3 is unreachable from
/// Lean code").
pub(crate) fn before_block() {
    if !alive() {
        hang_thread()
    }
    if super::reactor::in_no_suspend_scope() {
        panic!("{WAIT_IN_NO_SUSPEND}");
    }
}

/// The running context, or `None` once the scheduler's thread-local is
/// gone (W6). Before the task manager runs it is `main`'s, found without
/// building the scheduler's state, as `refs::frame` finds its frame: a
/// program that creates no tasks pays nothing for the scheduler at its
/// constants and thunks (lean2rr claims each constant through `step_keyed`
/// before `main`). There is one context then, since a task runs at once on
/// the context that creates it (`spawn`); also with `LEAN_NUM_THREADS=0`,
/// where the manager never runs. `manager_running` reads a const
/// thread-local with no destructor, which stays readable while the thread's
/// locals are destructed, so W6's answer is unchanged.
fn me() -> Option<CtxId> {
    if !super::manager_running() {
        return Some(MAIN);
    }
    alive().then(current_context)
}

/// Contexts woken in order (W1); nothing once the scheduler's state is gone
/// (W6). `wake` never switches. With no waiter it touches nothing: a store
/// that no context waits for builds no scheduler state, which matters
/// before the task manager runs (a context that waits has built it).
pub(crate) fn wake_in_order(ws: Vec<CtxId>) {
    if ws.is_empty() || !alive() {
        return;
    }
    for c in ws {
        wake(c);
    }
}

// ---------------------------------------------------------------------------
// WaitList

/// Contexts that wait for one event, woken together in the order they
/// began to wait (W1): the waiter list of a translator's object that has
/// room for one (the single-thread `sched::Ref`). 32 bytes. Not `Send`: it
/// names contexts of one thread's scheduler.
pub struct WaitList {
    waiters: RefCell<Vec<CtxId>>,
    _not_send: PhantomData<*const ()>,
}

impl WaitList {
    pub const fn new() -> WaitList {
        WaitList {
            waiters: RefCell::new(Vec::new()),
            _not_send: PhantomData,
        }
    }

    /// Register the running context, then block until `wake_all`: a
    /// blocking yield point (`block_sync`), and the context keeps its
    /// worker. The list is not borrowed while the context waits (W2).
    /// Afterwards the caller looks at its object again: the event may have
    /// been undone meanwhile (another waiter woken first took the value).
    /// Inside a no-suspend scope: a Rust panic, `WAIT_IN_NO_SUSPEND` (W3).
    /// Once the thread's locals are gone: the thread waits forever (W6).
    pub fn wait(&self) {
        before_block();
        let me = current_context();
        self.waiters.borrow_mut().push(me);
        block_sync();
    }

    /// Make every registered context runnable, in the order they began to
    /// wait. Never switches. The list is taken out in a borrow of its own
    /// and no translator code runs meanwhile (W2).
    pub fn wake_all(&self) {
        let ws = std::mem::take(&mut *self.waiters.borrow_mut());
        wake_in_order(ws);
    }

    /// Whether no context waits.
    pub fn is_empty(&self) -> bool {
        self.waiters.borrow().is_empty()
    }
}

impl Default for WaitList {
    fn default() -> WaitList {
        WaitList::new()
    }
}

// ---------------------------------------------------------------------------
// Gate

/// The runner of a computation that has no value yet, for a translator's
/// cell with 4 bytes to spare (leanrs's thunk cell and `LocalLazy`): a
/// `Cell<u32>`, the runner's context or none. Its waiters are kept in this
/// thread's keyed table under the cell's key, so the cell holds no list.
///
/// The contract a translator may rely on (leanrs's `unsafe impl Sync for
/// LocalLazy`, its proof Lem-LL):
/// - `Gate::new` is a `const fn`;
/// - `Gate` is `Send` (automatically) and not `Sync`, and has no `Drop`;
/// - its methods touch only the gate's own cell and the calling thread's
///   thread-locals (the keyed table, the scheduler's state); they never
///   dereference the key;
/// - no borrow is held while `finish`'s `store` runs, nor while the context
///   waits.
pub struct Gate {
    runner: Cell<u32>,
}

/// What `Gate::step` tells its caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The running context is now the runner: compute the value, then call
    /// `Gate::finish`.
    Run,
    /// A wait ended: look at the value again.
    Again,
}

impl Gate {
    /// A gate with no runner.
    pub const fn new() -> Gate {
        Gate {
            runner: Cell::new(NO_RUNNER),
        }
    }

    /// The cell whose key is `key` (its address) has no value: the rules
    /// of the module.
    /// - No runner: the running context becomes the runner (`Step::Run`).
    /// - The running context is the runner: `hang()`, never returns
    ///   (LB-08).
    /// - Another context is the runner: the running context waits until
    ///   `finish` (a blocking yield point; it keeps its worker), then
    ///   `Step::Again`.
    ///
    /// A hang or a wait inside a no-suspend scope is a Rust panic (W3).
    /// Before the task manager runs, the running context is `main`'s, and
    /// a step with no runner builds no scheduler state (`me`). At thread
    /// teardown (W6) the thread counts as `main`'s context: it claims a
    /// gate with no runner, and a recorded runner makes it hang, as
    /// `step_keyed` does.
    ///
    /// Example (a thunk): `loop { if let Some(v) = cell.value.get() {
    /// return v } match cell.gate.step(key) { Step::Again => continue,
    /// Step::Run => { let v = f(); cell.gate.finish(key, || cell.value.set(v)) } } }`.
    #[cold]
    pub fn step(&self, key: usize) -> Step {
        debug_assert!(
            key & 1 == 0,
            "lean-runtime: a Gate's key is an address (even)"
        );
        // before the task manager runs, and at thread teardown (W6), the
        // thread is `main`'s context (`me`): it claims a gate with no
        // runner, and a recorded runner makes it hang (`before_block`), as
        // `step_keyed` does
        let me = me().unwrap_or(MAIN);
        match self.runner.get() {
            NO_RUNNER => {
                self.runner.set(me.index());
                Step::Run
            }
            r if r == me.index() => {
                before_block();
                hang()
            }
            _ => {
                before_block();
                keyed_wait(key, me);
                Step::Again
            }
        }
    }

    /// The runner stores the value: `before_publish()` (a writers point:
    /// the streams the context's drops handed off are delivered first, as
    /// natively the computation's `fclose`s returned before its value was
    /// seen), then `store()`, then the gate has no runner, then the
    /// waiters under `key` wake in order (W1). The order is fixed here, so
    /// a waiter never sees the value before the writers point. No borrow is
    /// held while `store` runs.
    pub fn finish(&self, key: usize, store: impl FnOnce()) {
        before_publish();
        store();
        self.runner.set(NO_RUNNER);
        done_keyed(key);
    }

    /// Whether a context runs the computation now.
    pub fn running(&self) -> bool {
        self.runner.get() != NO_RUNNER
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new()
    }
}

// ---------------------------------------------------------------------------
// The keyed table

/// A computation with an entry in the keyed table: its key, its runner if
/// the table records one (the keyed claims; a `Gate`'s runner is in the
/// gate), and the contexts waiting for its value, in the order they began
/// to wait.
struct Keyed {
    key: usize,
    runner: u32,
    waiters: Vec<CtxId>,
}

/// A thread's table of entries, each found by its key (the keyed table
/// here, `ref_keyed`'s in `refs.rs`): the first `N` entries in places of
/// their own, the others in a `Vec`. So a thread whose table never holds
/// more than `N` entries at a time allocates nothing for it (AR-40:
/// lean2rr's `main` thread claims constants, and the first claim's
/// allocation of a `Vec` shifted the layout of its heap). The order of the
/// entries is not kept: a key has at most one entry.
pub(super) struct Table<E, const N: usize> {
    inline: [Option<E>; N],
    spill: Vec<E>,
}

impl<E, const N: usize> Table<E, N> {
    pub(super) const fn new() -> Self {
        Table {
            inline: [const { None }; N],
            spill: Vec::new(),
        }
    }

    /// The number of entries.
    pub(super) fn len(&self) -> usize {
        self.inline.iter().filter(|e| e.is_some()).count() + self.spill.len()
    }

    /// The first entry `which` accepts.
    pub(super) fn find(&self, which: impl Fn(&E) -> bool) -> Option<&E> {
        self.inline
            .iter()
            .flatten()
            .chain(&self.spill)
            .find(|e| which(e))
    }

    /// The same, to change it.
    pub(super) fn find_mut(&mut self, which: impl Fn(&E) -> bool) -> Option<&mut E> {
        self.inline
            .iter_mut()
            .flatten()
            .chain(&mut self.spill)
            .find(|e| which(e))
    }

    /// Add `e` in the first free place, or at the end of the `Vec`.
    pub(super) fn push(&mut self, e: E) {
        match self.inline.iter_mut().find(|p| p.is_none()) {
            Some(p) => *p = Some(e),
            None => self.spill.push(e),
        }
    }

    /// Take out the first entry `which` accepts.
    pub(super) fn remove(&mut self, which: impl Fn(&E) -> bool) -> Option<E> {
        if let Some(p) = self
            .inline
            .iter_mut()
            .find(|p| p.as_ref().is_some_and(&which))
        {
            return p.take();
        }
        let k = self.spill.iter().position(which)?;
        Some(self.spill.swap_remove(k))
    }
}

thread_local! {
    /// The computations that have a runner in the table or a waiter: an
    /// entry lives from a keyed claim or the first wait to the value's
    /// store (`done_keyed`). Usually empty, or a few entries during a
    /// constant's initialization (one per constant whose initialization
    /// is in progress), so the slow paths scan it. Its first 8 entries need
    /// no allocation (AR-40). It has a destructor (the waiter lists), so
    /// it is gone at thread teardown (W6).
    static KEYED: RefCell<Table<Keyed, 8>> = const { RefCell::new(Table::new()) };
    /// `KEYED`'s length, for the inline test of `done_keyed`.
    static KEYED_LEN: Cell<u32> = const { Cell::new(0) };
}

fn set_len(t: &Table<Keyed, 8>) {
    let _ = KEYED_LEN.try_with(|n| n.set(t.len() as u32));
}

/// Register context `me` as a waiter for `key` (an entry with no runner if
/// none exists), then block until the value's store wakes it.
fn keyed_wait(key: usize, me: CtxId) {
    let registered = KEYED
        .try_with(|t| {
            let mut t = t.borrow_mut();
            match t.find_mut(|e| e.key == key) {
                Some(e) => e.waiters.push(me),
                None => {
                    t.push(Keyed {
                        key,
                        runner: NO_RUNNER,
                        waiters: vec![me],
                    });
                    set_len(&t);
                }
            }
        })
        .is_ok();
    if !registered {
        // W6: nothing could wake this context.
        hang_thread()
    }
    block_sync();
}

/// [`Gate::step`] for a cell with no room for a gate (lean2rr's constant
/// slots, keyed `(slot << 1) | 1`): the runner is recorded in this thread's
/// keyed table. `true`: the running context is now the runner (compute,
/// store, then `done_keyed(key)`). `false`: a wait for another context's
/// run ended; look at the value again. Never returns when the running
/// context is the runner (`hang()`, LB-08). A hang or a wait inside a
/// no-suspend scope is a Rust panic (W3), which aborts here: the function
/// is `extern "C"` (it cannot unwind; W4). Before the task manager runs,
/// a claim and its `done_keyed` build no scheduler state (`me`,
/// `wake_in_order`). At thread teardown (W6) a recorded runner makes the
/// thread hang, and with the table gone (no runner known) it claims:
/// `true`; `Gate::step` makes the same choices.
///
/// Example (lean2rr's constant accessor, its cold path): `loop { if
/// has(slot) { return true } if step_keyed(key(slot)) { return false } }`.
#[cold]
#[inline(never)]
pub extern "C" fn step_keyed(key: usize) -> bool {
    enum S {
        Run,
        Hang,
        Wait,
    }
    // before the task manager runs, and at thread teardown (W6), the
    // thread is `main`'s context (`me`), as in `Gate::step`: a recorded
    // runner makes it hang (`before_block`); with the table gone no runner
    // is known, and it claims
    let me = me().unwrap_or(MAIN);
    let s = KEYED.try_with(|t| {
        let mut t = t.borrow_mut();
        match t.find_mut(|e| e.key == key) {
            None => {
                t.push(Keyed {
                    key,
                    runner: me.index(),
                    waiters: Vec::new(),
                });
                set_len(&t);
                S::Run
            }
            Some(e) if e.runner == NO_RUNNER => {
                e.runner = me.index();
                S::Run
            }
            Some(e) if e.runner == me.index() => S::Hang,
            Some(_) => S::Wait,
        }
    });
    match s {
        Ok(S::Run) | Err(_) => true,
        Ok(S::Hang) => {
            before_block();
            hang()
        }
        Ok(S::Wait) => {
            before_block();
            keyed_wait(key, me);
            false
        }
    }
}

/// A cell says "running" but records no runner (lean2rr's `busy` thunk,
/// whose generated state does not name the forcer): wait for the value's
/// store (`done_keyed(key)`), then return (look again); or never return.
/// - If the keyed table records a runner for `key`, `step_keyed`'s rules
///   apply: the running context's own run hangs, another's is waited for.
/// - Otherwise, before the task manager runs, or with no other live
///   context, the running context must be the runner: `hang()` (LB-08).
/// - Otherwise it waits. If it is in fact its own runner (its computation
///   blocked, then forced itself again), it waits forever, which keeps its
///   worker as the hang does and cannot be told apart from it (judge's
///   verdict on audit divergence 2).
///
/// A hang or a wait inside a no-suspend scope is a Rust panic (W3), which
/// aborts here (`extern "C"`; W4).
#[cold]
#[inline(never)]
pub extern "C" fn wait_running_keyed(key: usize) {
    debug_assert!(
        key & 1 == 0,
        "lean-runtime: an object's key is its address (even)"
    );
    before_block();
    if !super::manager_running() || with(|s| s.cx.live() <= 1) {
        hang()
    }
    let me = current_context();
    let own = KEYED
        .try_with(|t| {
            t.borrow()
                .find(|e| e.key == key && e.runner == me.index())
                .is_some()
        })
        .unwrap_or(false);
    if own {
        hang()
    }
    keyed_wait(key, me);
}

/// The value under `key` is stored: its entry goes (the runner the table
/// recorded, if any) and its waiters wake in order (W1). Never switches.
/// One thread-local load when the table is empty, which is almost always
/// (W4); the rest is out of line and cannot unwind. The writers point
/// before the store is the caller's (`before_publish()`), as
/// `Gate::finish` makes it.
#[inline]
pub fn done_keyed(key: usize) {
    if KEYED_LEN.try_with(Cell::get).unwrap_or(0) != 0 {
        done_keyed_slow(key);
    }
}

#[cold]
#[inline(never)]
extern "C" fn done_keyed_slow(key: usize) {
    let ws = KEYED
        .try_with(|t| {
            let mut t = t.borrow_mut();
            let e = t.remove(|e| e.key == key)?;
            set_len(&t);
            Some(e.waiters)
        })
        .ok()
        .flatten();
    if let Some(ws) = ws {
        wake_in_order(ws);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Whether this thread built the scheduler's state (`SCHED`), asked
    /// without building it.
    pub(crate) fn sched_built() -> bool {
        super::super::ctx::BUILT.with(Cell::get)
    }

    /// The number of entries of the keyed table.
    pub(crate) fn keyed_len() -> u32 {
        KEYED_LEN.with(Cell::get)
    }

    /// The runner the keyed table records for `key`, and its waiters.
    pub(crate) fn keyed_entry(key: usize) -> Option<(Option<u32>, Vec<CtxId>)> {
        KEYED.with(|t| {
            t.borrow().find(|e| e.key == key).map(|e| {
                (
                    (e.runner != NO_RUNNER).then_some(e.runner),
                    e.waiters.clone(),
                )
            })
        })
    }

    /// Record context number `r` as the runner of keyed claim `key`.
    pub(crate) fn set_keyed_runner(key: usize, r: u32) {
        KEYED.with(|t| {
            let mut t = t.borrow_mut();
            t.push(Keyed {
                key,
                runner: r,
                waiters: Vec::new(),
            });
            set_len(&t);
        });
    }

    /// Record context number `r` as `g`'s runner (a context that blocked in
    /// its computation, which the unit tests cannot suspend).
    pub(crate) fn set_runner(g: &Gate, r: u32) {
        g.runner.set(r);
    }

    #[test]
    fn sizes() {
        assert_eq!(std::mem::size_of::<Gate>(), 4);
        assert_eq!(std::mem::size_of::<WaitList>(), 32);
    }
}
