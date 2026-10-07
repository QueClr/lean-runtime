//! Core 3.3 of the wait cores (batch wait-1, `docs/sched.md`, "The wait
//! cores"): the resolution of promises dropped inside a translator's free,
//! put off until the free is over. Compiled in both modes, as `common.rs`
//! is: everything here is per thread, and it calls the build's own
//! `enter_no_suspend`, `leave_no_suspend` and `resolve`.
//!
//! Natively, when the last reference to an unresolved promise goes inside a
//! free, `lean_del_core` resolves it with `none` right there
//! (`deactivate_promise`, `object.cpp` 1353-1357), and its `sync`
//! dependents run inside the free, on the freeing thread, before the free
//! reaches the next object; a free inside a dependent is a new
//! `lean_dec_ref_cold` with its own list, so the promises it frees are
//! resolved inside that dependent. A translator's free must not suspend
//! (lean2rr's Reussir drain is the thread's, leanrs's drain must not
//! switch), but a `sync` dependent is Lean code that may block. So:
//! - R1: every step that runs inside a drain runs in the no-suspend scope
//!   (`DrainScope` enters it);
//! - R2: nothing in the scope suspends: the writers points do not wait
//!   there, a dropped stream's close hands its bytes off, the wait cores
//!   panic instead of blocking (W3), and `resolve` asserts in debug builds
//!   that it is not called there;
//! - R3: a promise resolution reached inside a drain is deferred (`defer`),
//!   never run there. The slot's store may happen in the drain (lean2rr's
//!   `Deferred::Resolve`) or with the resolution (leanrs's
//!   `Deferred::Call`). The two differ once an earlier entry's dependent
//!   blocks: other contexts then run, and with the store made in the drain
//!   they see a later promise's `none` (its slot, `IO.getTaskState`
//!   `finished`) before its dependents have run, where natively that
//!   promise is not resolved yet; with the store made with the resolution
//!   they see it unresolved, as natively;
//! - R4: the deferred list runs only after the drain has ended and the
//!   scope has been left, through `run_deferred()`: `DrainScope`'s
//!   outermost drop calls it, a translator with drains of its own
//!   (lean2rr's Reussir drains) calls it at each drain's end, then the
//!   drain-end hook `after_drain()` (`DrainScope` calls both). Each entry
//!   first waits for the writers of the streams the context handed off
//!   before it (its mark), and runs with the writers of the streams its
//!   drain handed off after it skipped, as natively the free reached the
//!   promise before their `fclose` (review RF14-07); `after_drain` then
//!   waits for the rest (review HR-01..03). That is native's outcome only
//!   when the translator drops and defers in native's free order
//!   (`lean_del_core`'s LIFO): a translator that frees in another order
//!   (Rust drop glue) gets the outcome that order implies, which can be
//!   native's mirror (leanrs's DV11);
//!   `leave_no_suspend` never runs it (AR-8). While a panic unwinds nothing
//!   runs, and the entries stay queued for the next `run_deferred()`;
//! - R5: each entry is tagged with the context that deferred it, and a
//!   `run_deferred()` moves out the running context's entries only, then
//!   walks them in push order (the order of the last drops), on the
//!   dropping context; after each entry it runs the running context's
//!   entries queued meanwhile, before the walk's next entry. A drain inside
//!   a deferred resolution (a dependent that frees more promises) starts
//!   with no entry of its own, and its promises are resolved before the
//!   outer walk's next entry, at its own end or, if the translator did not
//!   report that end, right after the dependent: native's order. In the
//!   scenario where a free drops `pA` then `pB`, and `pA`'s dependent frees
//!   a container holding `pC`, `pC` is resolved before `pB` if and only if
//!   `pA` is (case `tasks/promise_nested_free_order`). If a dependent never
//!   returns (`Promise.result!` of a dropped promise), the rest of the
//!   moved entries never run, as natively that free never resumes; the
//!   thread's list is clean for the other contexts, since they were moved
//!   out;
//! - R6: the list is empty at every context switch and when a context ends,
//!   which debug builds check (single-thread scheduler). A test that catches
//!   a panic out of a drain or a walk and then switches with entries queued
//!   makes the check fire (accepted).
//!
//! **A drain whose end is not reported** (lean2rr's Reussir drains without
//! its patch 0040, where the translator runs the list at its next effect
//! point instead) breaks R6. In release builds its entries then stay
//! queued, tagged with their context: they run at that context's next
//! `run_deferred()` (a drain's end, a settle point), never on another
//! context, or, at the latest, when the context ends (`main`'s: at
//! `finish`); until then their
//! promises look unresolved and their dependents have not run, where
//! natively they ran inside the free. A translator must report every
//! drain's end before it adopts this core (lean2rr: make patch 0040
//! required, its L5).
//!
//! The one difference from native: a dependent runs after all of its
//! drain's frees, where natively it runs after the frees that precede its
//! promise (leanrs: DV11; lean2rr: its plan §10).
//!
//! **At thread teardown** the list stays: it has no destructor, so a drain
//! in a thread-local's destructor still defers, and its scope's end runs
//! the entry outside the scope. Entries still queued when the thread ends
//! are never run nor dropped, as native Lean frees nothing at exit.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;

use super::TaskId;

thread_local! {
    /// How many drains of the running context are running (`DrainScope`s
    /// nest). Set aside while the context is switched away
    /// (`park_depth`), as the no-suspend depth is.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// The deferred resolutions, in push order, each with the context that
    /// deferred it (`tag`) and its writer mark (`writer_mark`: the streams
    /// the drain handed off before it have lower ids, review RF14-07). No
    /// destructor (see the module's teardown note).
    static LIST: RefCell<ManuallyDrop<Vec<Entry>>> =
        const { RefCell::new(ManuallyDrop::new(Vec::new())) };
    /// `LIST`'s length, for `DrainScope`'s inline test.
    static QUEUED: Cell<u32> = const { Cell::new(0) };
    /// The deferred resolutions that have not finished: queued in `LIST`,
    /// or moved out by a `run_deferred` that is still walking them (in
    /// flight; one whose dependent blocks stays counted until it returns,
    /// and one that never returns stays counted, with the rest of its
    /// walk). `deferred_pending()` reads it.
    static PENDING: Cell<u32> = const { Cell::new(0) };
}

/// A deferred resolution in `LIST`: its context (`tag`), the resolution,
/// and its writer mark (`writer_mark`).
type Entry = (u32, Deferred, u64);

/// The id the next stream hand-off will get (`io::coop::writer_mark`): an
/// entry's mark, below which are the writers handed off before it.
#[cfg(all(feature = "sched", feature = "io"))]
fn writer_mark() -> u64 {
    crate::io::coop::writer_mark()
}

#[cfg(not(all(feature = "sched", feature = "io")))]
fn writer_mark() -> u64 {
    0
}

/// While it lives, the writers points of this thread skip the writers with
/// ids in `lo..hi` (`io::coop::skip_writers`).
#[cfg(all(feature = "sched", feature = "io"))]
fn skip_writers(lo: u64, hi: u64) -> Option<crate::io::coop::SkipWriters> {
    crate::io::coop::skip_writers(lo, hi)
}

/// Without the io layer's hand-offs (threads mode, `sched` without `io`)
/// there is no writer to skip.
#[cfg(not(all(feature = "sched", feature = "io")))]
fn skip_writers(_lo: u64, _hi: u64) -> Option<std::convert::Infallible> {
    None
}

/// The context an entry belongs to: the running context's number on the
/// single-thread scheduler (`main`'s before the task manager runs, when it
/// is the only one, and once the thread's locals are gone); 0 in threads
/// mode, where the thread is the context.
#[cfg(feature = "sched")]
fn tag() -> u32 {
    if super::manager_running() && super::alive() {
        super::current_context().index()
    } else {
        super::MAIN.index()
    }
}

#[cfg(not(feature = "sched"))]
fn tag() -> u32 {
    0
}

fn add(cell: &'static std::thread::LocalKey<Cell<u32>>, n: i64) {
    cell.with(|c| c.set((i64::from(c.get()) + n).max(0) as u32));
}

/// The message of R2's debug check: a promise resolved inside a
/// no-suspend scope (a translator's free).
pub const RESOLVE_IN_NO_SUSPEND: &str =
    "lean-runtime: a promise resolved inside a no-suspend scope (a free): defer it (sched::defer)";

/// A translator's drop drain on this thread (leanrs's `Deep` and `Dyn`
/// drains), as a guard: the outermost `enter` enters the no-suspend scope
/// (R1); the outermost drop leaves it, then runs the deferred resolutions
/// (`run_deferred`, R4). Nests. Not `Send`: the depth is the thread's (and
/// the running context's: a switch sets it aside).
#[must_use = "the drain ends when the scope is dropped"]
pub struct DrainScope {
    _not_send: PhantomData<*const ()>,
}

impl DrainScope {
    /// The depth plus one; the outermost entry calls `enter_no_suspend()`.
    #[inline]
    pub fn enter() -> DrainScope {
        let outer = DEPTH.with(|d| {
            let n = d.get();
            d.set(n + 1);
            n == 0
        });
        if outer {
            super::enter_no_suspend();
        }
        DrainScope {
            _not_send: PhantomData,
        }
    }

    /// Whether a drain of the running context runs.
    #[inline]
    pub fn active() -> bool {
        DEPTH.with(|d| d.get() > 0)
    }
}

impl Drop for DrainScope {
    /// The depth minus one. The outermost exit calls `leave_no_suspend()`,
    /// then, if an entry is queued, `run_deferred()` (each entry waits for
    /// the writers handed off before it, review RF14-07), except while a
    /// panic unwinds (the entries stay queued for the next
    /// `run_deferred()`, R4), then the drain-end hook (`after_drain`: the
    /// writers of the other streams the drain handed off end, review
    /// HR-01..03). This drop may switch (a writers' wait, a deferred
    /// resolution's dependent that blocks): a translator drops the scope
    /// only where a switch is allowed, at the end of the drop that started
    /// the drain; it switches whenever the context has a writer that runs.
    /// Two relaxed or thread-local loads more while no writer runs in the
    /// process and nothing is queued.
    #[inline]
    fn drop(&mut self) {
        let last = DEPTH.with(|d| {
            let n = d.get().saturating_sub(1);
            d.set(n);
            n == 0
        });
        if !last {
            return;
        }
        super::leave_no_suspend();
        if QUEUED.with(Cell::get) > 0 && !std::thread::panicking() {
            run_deferred();
        }
        // waits for nothing while a panic unwinds
        super::after_drain();
    }
}

/// A resolution put off to the end of a drain (`defer`).
pub enum Deferred {
    /// `resolve(id, || {})`: the glue has already stored the value in its
    /// slot, in the drain's order (lean2rr).
    Resolve(TaskId),
    /// The glue's own resolution, its store included (leanrs's closure,
    /// which calls `resolve` itself). Like a `Job`, it may hold translator
    /// values; it runs, or is dropped, with no borrow of the crate held
    /// (W2), on the thread that pushed it (so no `Send` in threads mode).
    Call(Box<dyn FnOnce()>),
}

impl Deferred {
    fn run(self) {
        match self {
            Deferred::Resolve(id) => {
                super::resolve(id, || {});
            }
            Deferred::Call(f) => f(),
        }
    }
}

/// Put a promise's resolution off to the end of the running drain (R3),
/// tagged with the running context: it runs at that context's next
/// `run_deferred()`, after the drain. It records the writer mark: the
/// streams the drain hands off after it are the ones its resolution does
/// not wait for (review RF14-07).
pub fn defer(d: Deferred) {
    let t = tag();
    let m = writer_mark();
    LIST.with(|l| l.borrow_mut().push((t, d, m)));
    add(&QUEUED, 1);
    add(&PENDING, 1);
}

/// Whether a deferred resolution of this thread has not finished: queued,
/// or in flight in a `run_deferred` walk (one whose dependent blocked, and
/// the rest of that walk behind it). One thread-local load. A translator's
/// "every task has settled" test reads it (lean2rr's `settled`), so a
/// promise moved out but not yet resolved still counts; after a walk whose
/// dependent never returns it stays true.
#[inline]
pub fn deferred_pending() -> bool {
    PENDING.with(|n| n.get() > 0)
}

/// Move the running context's deferred entries out of this thread's list,
/// then run each in push order (R4, R5): `Resolve(id)` calls `resolve(id,
/// || {})`, `Call(f)` calls `f`. After each entry, the running context's
/// entries queued meanwhile (by a drain whose end was not reported) run
/// before the next. An entry's dependents may block and switch, and may
/// free more promises (new entries, resolved at their drain's end). Call it
/// after the drain has ended and the scope has been left. Inside a
/// no-suspend scope it is a debug assertion; in release builds it runs
/// nothing there, and the entries stay queued. A Rust panic out of an entry
/// puts the entries not yet run back on the thread's list, after the ones
/// queued meanwhile, for the next `run_deferred()` (R4).
#[cold]
#[inline(never)]
pub fn run_deferred() {
    let me = tag();
    let list = LIST.with(|l| {
        let mut l = l.borrow_mut();
        if !l.iter().any(|(t, ..)| *t == me) {
            None
        } else if super::in_no_suspend_scope() {
            Some(None)
        } else {
            let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut **l)
                .into_iter()
                .partition(|(t, ..)| *t == me);
            **l = rest;
            Some(Some(mine))
        }
    });
    let list = match list {
        Some(Some(list)) => list,
        Some(None) => {
            if cfg!(debug_assertions) {
                panic!("lean-runtime: run_deferred inside a no-suspend scope (R4)");
            }
            return;
        }
        None => return,
    };
    add(&QUEUED, -(list.len() as i64));
    // Each entry waits first for the context's writers but the ones its
    // drain handed off after it (ids from its mark up to now), as natively
    // the free reached the promise before those streams' `fclose`, then it
    // runs with those writers skipped by every writers point, its `sync`
    // dependents' included (review RF14-07). The list is moved out, so a
    // switch during a wait leaves no entry queued (R6). The drain's end
    // (`after_drain`) waits for the rest afterwards.
    let now = writer_mark();
    let mut walk = Walk(list.into_iter());
    for (_, d, mark) in walk.0.by_ref() {
        let _skip = skip_writers(mark, now);
        super::after_drain();
        {
            let _done = Done;
            d.run();
        }
        // the entries a drain inside that one queued, if its end was not
        // reported: before the walk's next entry, as natively
        run_deferred();
    }
}

/// The entries of a `run_deferred` walk not run yet: on a panic's
/// unwinding, back to the thread's list, after the ones queued meanwhile
/// (those came from inside the entry that panicked, so they come first
/// natively). They stay counted in `PENDING`.
struct Walk(std::vec::IntoIter<Entry>);

impl Drop for Walk {
    fn drop(&mut self) {
        let rest: Vec<Entry> = self.0.by_ref().collect();
        if rest.is_empty() {
            return;
        }
        add(&QUEUED, rest.len() as i64);
        LIST.with(|l| l.borrow_mut().extend(rest));
    }
}

/// One entry of a walk has finished, or unwound: no longer pending.
struct Done;

impl Drop for Done {
    fn drop(&mut self) {
        add(&PENDING, -1);
    }
}

/// Whether this thread's deferred list is empty (R6's debug check at a
/// context switch).
#[cfg(feature = "sched")]
pub(crate) fn queue_empty() -> bool {
    QUEUED.with(Cell::get) == 0
}

/// A context ends, at the end of its function, or `main`'s, at `finish`
/// (single-thread scheduler).
/// R6: no entry of it is queued; a drain whose end was not reported left
/// some. They run here, on their own context, before it ends (never on
/// another one, nor on a later context that reuses its number); then debug
/// builds report the glue's missing `run_deferred()`.
#[cfg(feature = "sched")]
pub(crate) fn context_ends() {
    let me = tag();
    let left = LIST.with(|l| l.borrow().iter().any(|(t, ..)| *t == me));
    if left {
        run_deferred();
    }
    debug_assert!(
        !left,
        "lean-runtime: deferred promise resolutions queued when a context ends (R6): a drain's end was not reported (run_deferred)"
    );
}

/// The running context's drain depth, set aside while it is switched away
/// (`switch_away`): the other contexts run outside its drain, and it is in
/// its drain again when it goes on, as the no-suspend depth (RSIO-10;
/// review RW1-02).
#[cfg(feature = "sched")]
pub(crate) fn park_depth() -> DepthParked {
    DepthParked(DEPTH.with(|d| d.replace(0)))
}

/// A suspended context's drain depth (`park_depth`), given back when it goes
/// on, or when a panic unwinds its switch.
#[cfg(feature = "sched")]
pub(crate) struct DepthParked(u32);

#[cfg(feature = "sched")]
impl Drop for DepthParked {
    fn drop(&mut self) {
        let d = self.0;
        DEPTH.with(|c| c.set(d));
    }
}

/// R2's debug check, at the start of `resolve` in both modes.
#[inline]
pub(crate) fn check_resolve_outside_scope() {
    debug_assert!(!super::in_no_suspend_scope(), "{RESOLVE_IN_NO_SUSPEND}");
}

/// Test bodies run in both modes: `sched/tests.rs` (the single-thread
/// scheduler) and `sched/mt/tests.rs` (threads mode) call them once their
/// task manager runs. The `sync` dependents of a promise run on the
/// resolving thread in both modes, so these tests switch no context.
#[cfg(test)]
pub(crate) mod tests {
    use super::super::{
        depend, enter_no_suspend, in_no_suspend, leave_no_suspend, promise_new, resolve, Job,
        Outcome, TaskId,
    };
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    /// Drop the queued entries without running them (a walk that hung
    /// keeps its entries on its context's stack: the model test discards
    /// the ones its simulated hang put back).
    #[cfg(feature = "sched")]
    pub(crate) fn discard_queued() {
        let list = LIST.with(|l| std::mem::take(&mut **l.borrow_mut()));
        add(&QUEUED, -(list.len() as i64));
        add(&PENDING, -(list.len() as i64));
        drop(list);
    }

    fn log() -> Log {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn entries(l: &Log) -> Vec<String> {
        l.lock().unwrap().clone()
    }

    /// A promise with one `sync` dependent that runs `f` and logs `name`
    /// (and whether it ran inside a drain or a no-suspend scope).
    fn promise(l: &Log, name: &'static str, f: impl FnOnce() + Send + 'static) -> TaskId {
        let p = promise_new().expect("the task manager runs");
        let l = l.clone();
        let job: Job = Box::new(move || {
            f();
            let inside = DrainScope::active() || in_no_suspend();
            l.lock().unwrap().push(if inside {
                format!("{name} inside")
            } else {
                name.to_string()
            });
            Outcome::Done
        });
        depend(p, job, 0, true, true);
        p
    }

    /// A drain defers two promises: nothing runs inside it; at its end
    /// they resolve in drop order, outside the scope.
    pub(crate) fn resolutions_run_after_the_drain_in_drop_order() {
        let l = log();
        let a = promise(&l, "A", || {});
        let b = promise(&l, "B", || {});
        {
            let _d = DrainScope::enter();
            assert!(DrainScope::active() && in_no_suspend());
            defer(Deferred::Call(Box::new(move || {
                resolve(a, || {});
            })));
            defer(Deferred::Resolve(b));
            assert!(deferred_pending());
            assert!(entries(&l).is_empty());
        }
        assert_eq!(entries(&l), ["A", "B"]);
        assert!(!deferred_pending() && !DrainScope::active() && !in_no_suspend());
    }

    /// R5: a drain inside a deferred resolution (`pA`'s dependent frees a
    /// container holding `pC`) resolves its own promise before the outer
    /// walk's next entry: C, A, B (native's order; the judge's verdict on
    /// the nested-free order).
    pub(crate) fn a_nested_drain_resolves_its_own_promises_first() {
        let l = log();
        let c = promise(&l, "C", || {});
        let a = promise(&l, "A", move || {
            let _inner = DrainScope::enter();
            defer(Deferred::Resolve(c));
        });
        let b = promise(&l, "B", || {});
        {
            let _d = DrainScope::enter();
            defer(Deferred::Resolve(a));
            defer(Deferred::Resolve(b));
        }
        assert_eq!(entries(&l), ["C", "A", "B"]);
        assert!(!deferred_pending());
    }

    /// The in-flight count: while an entry of a walk runs, the thread's list
    /// is empty (moved out), but the walk's entries still count as pending,
    /// so a "settled" test sees them (the judge's caveat).
    pub(crate) fn entries_moved_out_still_count_as_pending() {
        let l = log();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let a = promise(&l, "A", move || {
            let queued = LIST.with(|q| q.borrow().len());
            s2.lock().unwrap().push((queued, deferred_pending()));
        });
        let b = promise(&l, "B", || {});
        defer(Deferred::Resolve(a));
        defer(Deferred::Resolve(b));
        assert_eq!(PENDING.with(Cell::get), 2);
        run_deferred();
        assert_eq!(*seen.lock().unwrap(), [(0, true)]);
        assert_eq!(entries(&l), ["A", "B"]);
        assert!(!deferred_pending());
        // an empty list: nothing to do
        run_deferred();
    }

    /// R4 and F2: a panic unwinding through a drain runs nothing, and leaves
    /// the drain and the scope; the next `run_deferred` resolves the entry.
    pub(crate) fn a_panic_through_a_drain_leaves_the_entries_queued() {
        let l = log();
        let a = promise(&l, "A", || {});
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _d = DrainScope::enter();
            defer(Deferred::Resolve(a));
            panic!("a translator's bug inside the drain");
        }));
        assert!(r.is_err());
        assert!(!DrainScope::active() && !in_no_suspend());
        assert!(entries(&l).is_empty());
        assert!(deferred_pending());
        run_deferred();
        assert_eq!(entries(&l), ["A"]);
        assert!(!deferred_pending());
    }

    /// R4: a panic out of an entry of a walk puts the entries not run yet
    /// back on the list, after the ones queued meanwhile.
    pub(crate) fn a_panic_out_of_an_entry_requeues_the_rest() {
        let l = log();
        let b = promise(&l, "B", || {});
        let c = promise(&l, "C", || {});
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _d = DrainScope::enter();
            defer(Deferred::Call(Box::new(move || {
                // queued meanwhile: comes first natively
                defer(Deferred::Resolve(c));
                panic!("a translator's bug in a resolution");
            })));
            defer(Deferred::Resolve(b));
        }));
        assert!(r.is_err());
        assert!(entries(&l).is_empty());
        assert_eq!(PENDING.with(Cell::get), 2);
        run_deferred();
        assert_eq!(entries(&l), ["C", "B"]);
        assert!(!deferred_pending());
    }

    /// R4: `run_deferred` inside a no-suspend scope is a debug assertion
    /// (it runs nothing in release builds).
    pub(crate) fn run_deferred_inside_a_scope() {
        let l = log();
        let a = promise(&l, "A", || {});
        defer(Deferred::Resolve(a));
        enter_no_suspend();
        let r = std::panic::catch_unwind(run_deferred);
        leave_no_suspend();
        assert_eq!(r.is_err(), cfg!(debug_assertions));
        assert!(entries(&l).is_empty());
        run_deferred();
        assert_eq!(entries(&l), ["A"]);
    }

    /// R2: `resolve` inside a no-suspend scope is a debug assertion.
    pub(crate) fn resolve_inside_a_scope() {
        let l = log();
        let a = promise(&l, "A", || {});
        enter_no_suspend();
        let r = std::panic::catch_unwind(|| resolve(a, || {}));
        leave_no_suspend();
        if cfg!(debug_assertions) {
            let e = r.expect_err("the debug check");
            let msg = e.downcast_ref::<String>().cloned().unwrap_or_default();
            assert_eq!(msg, RESOLVE_IN_NO_SUSPEND);
            assert!(entries(&l).is_empty());
            resolve(a, || {});
        }
        assert_eq!(entries(&l), ["A"]);
    }

    /// Nested scopes: only the outermost leaves the no-suspend scope and
    /// runs the list.
    pub(crate) fn scopes_nest() {
        let l = log();
        let a = promise(&l, "A", || {});
        {
            let _outer = DrainScope::enter();
            {
                let _inner = DrainScope::enter();
                defer(Deferred::Resolve(a));
            }
            assert!(DrainScope::active() && in_no_suspend());
            assert!(entries(&l).is_empty());
        }
        assert_eq!(entries(&l), ["A"]);
    }
}
