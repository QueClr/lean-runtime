//! `IO.Ref` (`ST.Ref`) in threads mode: Lean 4.35's rule (docs/threads.md,
//! 3.1; LB-01 and LB-18 in docs/lean-bugs.md) as a lock and a condition
//! variable.
//!
//! The rule:
//! - `modify` is `take`, then `put` (`ST.Prim.Ref.modifyUnsafe`): the
//!   reference is empty while its function runs;
//! - only `modify`'s own `put` fills the empty reference. Until then `get`,
//!   `take`, `set` and `swap` wait: the thread blocks, where native 4.35
//!   spins (same outcomes);
//! - `set` is `swap` with the result dropped (4.35's `Ref.set`), so a `set`
//!   is never lost (LB-01: native 4.34.0's `set` stores into the empty slot
//!   and `modify`'s store then overwrites it; its `get` puts an old value
//!   back over a newer one);
//! - `swap` returns the value the reference held (LB-18: native 4.34.0's
//!   returns its own argument on an empty slot);
//! - so `modify` and `swap` are atomic. The cost, as in 4.35: a `modify`
//!   whose function waits for a task that uses the same reference
//!   deadlocks.
//!
//! The reference is the translator's object (it holds the translator's
//! value `T`, as `Std.Sync`'s objects sit in the translator's handles): this
//! type is the rule's reference implementation, which a translator's
//! threads-mode `ST.Ref` may wrap (leanrs's point 3) or copy. A value is
//! cloned under the reference's own lock (`get`; `T::clone` must not use
//! this reference); a value it replaces is dropped after that lock is
//! released, so its destructor may do anything, also release a task. The
//! scheduler's lock is never held with it.
//!
//! A Rust panic in `modify`'s function leaves the reference empty; the
//! glue must end the process rather than go on (in a task, the panic aborts
//! it anyway).

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// `ST.Ref α` (`lean_st_ref_*`) under Lean 4.35's rule.
pub struct Ref<T> {
    /// `None` while `modify` holds it.
    cell: Mutex<Option<T>>,
    /// `modify`'s `put` filled it: whoever waits for a value looks again.
    filled: Condvar,
}

impl<T> Ref<T> {
    /// `ST.mkRef` (`lean_st_mk_ref`).
    pub fn new(v: T) -> Ref<T> {
        Ref {
            cell: Mutex::new(Some(v)),
            filled: Condvar::new(),
        }
    }

    /// The lock, once the reference holds a value.
    fn full(&self) -> MutexGuard<'_, Option<T>> {
        let mut c = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        while c.is_none() {
            c = self.filled.wait(c).unwrap_or_else(PoisonError::into_inner);
        }
        c
    }

    /// `ST.Ref.get` (`lean_st_ref_get`): a clone of the value, once the
    /// reference holds one. Never puts anything back.
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.full().clone().expect("a full reference")
    }

    /// `ST.Ref.take` (`lean_st_ref_take`): the value, once the reference
    /// holds one; the reference stays empty until `put`.
    pub fn take(&self) -> T {
        self.full().take().expect("a full reference")
    }

    /// Lean 4.35's `ST.Ref.put` (`lean_st_ref_put`): fill the reference that
    /// `take` emptied, and wake whoever waits for it. A put into a full
    /// reference (a glue's error; 4.35 asserts) replaces the value.
    pub fn put(&self, v: T) {
        let mut c = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        debug_assert!(c.is_none(), "lean-runtime: put into a full reference");
        let old = c.replace(v);
        drop(c);
        self.filled.notify_all();
        drop(old);
    }

    /// `ST.Ref.swap` (`lean_st_ref_swap`): the value the reference held,
    /// once it holds one, and `v` in its place.
    pub fn swap(&self, v: T) -> T {
        let mut c = self.full();
        let old = c.replace(v).expect("a full reference");
        drop(c);
        old
    }

    /// `ST.Ref.set` (`lean_st_ref_set`; 4.35: `discard <| Ref.swap r a`).
    /// The old value is dropped after the lock is released.
    pub fn set(&self, v: T) {
        drop(self.swap(v));
    }

    /// `ST.Ref.modify` (`Ref.modifyUnsafe`): `take`, `f` outside the lock,
    /// then `put`.
    pub fn modify(&self, f: impl FnOnce(T) -> T) {
        let v = self.take();
        self.put(f(v));
    }

    /// `ST.Ref.modifyGet` (`Ref.modifyGetUnsafe`): `take`, `f` outside the
    /// lock, then `put` of its second component; its first is returned.
    pub fn modify_get<R>(&self, f: impl FnOnce(T) -> (R, T)) -> R {
        let v = self.take();
        let (r, v) = f(v);
        self.put(v);
        r
    }
}
