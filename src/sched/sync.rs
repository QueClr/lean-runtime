//! `Std.Sync`'s primitives (`src/runtime/mutex.cpp`): `BaseMutex`
//! (`std::mutex`), `Condvar` (`std::condition_variable`),
//! `BaseRecursiveMutex` (`std::recursive_mutex`) and `BaseSharedMutex`
//! (libc++'s `std::shared_mutex`), on the scheduler's contexts. From lean2rr's
//! leanrt (`sync.rs`). `Std.Channel`, `Std.Barrier`, `Std.Semaphore` and the
//! rest of `Std.Sync` are Lean code over these and `IO.Promise`, so they need
//! nothing more here.
//!
//! A context that must wait (the mutex is held by another thread, a condition
//! variable before it is notified) blocks, and other contexts and queued
//! tasks run meanwhile, as other threads would. The owner of a lock is a
//! thread: a context, and on it the thread of the innermost running task (a
//! task needed by another runs on a worker thread natively, so it is another
//! thread than its caller; a `sync` dependent runs on its source's). As
//! natively (glibc), locking a `BaseMutex` that the same thread holds waits
//! forever, and unlocking one that is not locked by the caller just unlocks
//! it. A released mutex is handed to the thread that has waited longest.
//!
//! The objects are plain values; the glue keeps each in its own handle (an
//! `Rc`, or the translator's external object) and calls these methods with
//! it. Each method starts the scheduler first if [`super::start_lazy`] is
//! waiting for it (`ensure_started`): a lock's owner records whether the
//! scheduler has started, which tells an initializer's thread from `main`'s,
//! so every operation in `main` must see it started (lean2rr's review
//! RS4-05). A separate module, so that a translator can admit `Std.Sync` on
//! its own (decisions Q8).

use super::{block_sync, current_context, wake, with, CtxId};
use std::cell::RefCell;
use std::collections::VecDeque;

/// The thread that runs now (see the module comment): a context, the thread
/// of the innermost task running on it, and whether `main` has started
/// (module initializers run before the task manager, on another thread than
/// `main` natively).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Owner {
    ctx: CtxId,
    thread: u64,
    started: bool,
}

fn me() -> Owner {
    with(|s| Owner {
        ctx: s.cx.cur,
        thread: s.cur_thread(),
        started: s.tk.started,
    })
}

// ---------------------------------------------------------------------------
// BaseMutex

#[derive(Default)]
struct MutexState {
    owner: Option<Owner>,
    waiters: VecDeque<(CtxId, Owner)>,
}

/// `Std.BaseMutex` (`lean_io_basemutex_*`).
#[derive(Default)]
pub struct Mutex {
    st: RefCell<MutexState>,
}

impl Mutex {
    /// `lean_io_basemutex_new`.
    pub fn new() -> Mutex {
        super::ensure_started();
        Mutex::default()
    }

    fn lock_as(&self, who: Owner) {
        {
            let mut m = self.st.borrow_mut();
            if m.owner.is_none() {
                m.owner = Some(who);
                return;
            }
            // Held, by another thread or (a deadlock, as natively) by this one.
            m.waiters.push_back((current_context(), who));
        }
        // `unlock` hands it over.
        block_sync();
    }

    /// `lean_io_basemutex_lock`.
    pub fn lock(&self) {
        super::ensure_started();
        super::writers_point();
        self.lock_as(me());
    }

    /// `lean_io_basemutex_try_lock`.
    pub fn try_lock(&self) -> bool {
        super::ensure_started();
        let who = me();
        let mut m = self.st.borrow_mut();
        if m.owner.is_none() {
            m.owner = Some(who);
            true
        } else {
            false
        }
    }

    /// `lean_io_basemutex_unlock`. It switches only to let the context's
    /// handed-off streams end first (`writers_point`), before releasing.
    pub fn unlock(&self) {
        super::ensure_started();
        super::writers_point();
        self.unlock_inner();
    }

    /// The release itself; does not switch.
    fn unlock_inner(&self) {
        let next = {
            let mut m = self.st.borrow_mut();
            match m.waiters.pop_front() {
                Some((c, who)) => {
                    m.owner = Some(who);
                    Some(c)
                }
                None => {
                    m.owner = None;
                    None
                }
            }
        };
        if let Some(c) = next {
            wake(c);
        }
    }
}

// ---------------------------------------------------------------------------
// Condvar

/// `Std.Condvar` (`lean_io_condvar_*`).
#[derive(Default)]
pub struct Condvar {
    waiters: RefCell<VecDeque<CtxId>>,
}

impl Condvar {
    /// `lean_io_condvar_new`.
    pub fn new() -> Condvar {
        super::ensure_started();
        Condvar::default()
    }

    /// `lean_io_condvar_wait`: release `m`, wait to be notified, then take
    /// `m` again (natively `condition_variable::wait` on the adopted lock).
    pub fn wait(&self, m: &Mutex) {
        super::ensure_started();
        super::writers_point();
        let who = me();
        m.unlock_inner();
        self.waiters.borrow_mut().push_back(current_context());
        block_sync();
        m.lock_as(who);
    }

    /// `lean_io_condvar_notify_one`. It switches only to let the context's
    /// handed-off streams end first (`writers_point`).
    pub fn notify_one(&self) {
        super::ensure_started();
        super::writers_point();
        let w = self.waiters.borrow_mut().pop_front();
        if let Some(c) = w {
            wake(c);
        }
    }

    /// `lean_io_condvar_notify_all`. It switches only to let the context's
    /// handed-off streams end first (`writers_point`).
    pub fn notify_all(&self) {
        super::ensure_started();
        super::writers_point();
        let ws = std::mem::take(&mut *self.waiters.borrow_mut());
        for c in ws {
            wake(c);
        }
    }
}

// ---------------------------------------------------------------------------
// BaseRecursiveMutex

#[derive(Default)]
struct RecState {
    owner: Option<Owner>,
    count: u32,
    waiters: VecDeque<(CtxId, Owner)>,
}

/// `Std.BaseRecursiveMutex` (`lean_io_baserecmutex_*`).
#[derive(Default)]
pub struct RecursiveMutex {
    st: RefCell<RecState>,
}

impl RecursiveMutex {
    /// `lean_io_baserecmutex_new`.
    pub fn new() -> RecursiveMutex {
        super::ensure_started();
        RecursiveMutex::default()
    }

    /// `lean_io_baserecmutex_lock`.
    pub fn lock(&self) {
        super::ensure_started();
        super::writers_point();
        let who = me();
        {
            let mut m = self.st.borrow_mut();
            match m.owner {
                None => {
                    m.owner = Some(who);
                    m.count = 1;
                    return;
                }
                Some(o) if o == who => {
                    m.count += 1;
                    return;
                }
                Some(_) => m.waiters.push_back((current_context(), who)),
            }
        }
        block_sync();
    }

    /// `lean_io_baserecmutex_try_lock`.
    pub fn try_lock(&self) -> bool {
        super::ensure_started();
        let who = me();
        let mut m = self.st.borrow_mut();
        match m.owner {
            None => {
                m.owner = Some(who);
                m.count = 1;
                true
            }
            Some(o) if o == who => {
                m.count += 1;
                true
            }
            Some(_) => false,
        }
    }

    /// `lean_io_baserecmutex_unlock`. It switches only to let the context's
    /// handed-off streams end first (`writers_point`).
    pub fn unlock(&self) {
        super::ensure_started();
        super::writers_point();
        let next = {
            let mut m = self.st.borrow_mut();
            if m.count > 1 {
                m.count -= 1;
                return;
            }
            match m.waiters.pop_front() {
                Some((c, who)) => {
                    m.owner = Some(who);
                    m.count = 1;
                    Some(c)
                }
                None => {
                    m.owner = None;
                    m.count = 0;
                    None
                }
            }
        };
        if let Some(c) = next {
            wake(c);
        }
    }
}

// ---------------------------------------------------------------------------
// BaseSharedMutex: libc++'s `__shared_mutex_base` (Lean's runtime is built
// with libc++): a writer that has entered (`write_entered`) keeps new
// readers out and waits for the readers inside to leave.

#[derive(Default)]
struct SharedState {
    write_entered: bool,
    readers: u32,
    /// Waiting to enter (writers and readers): libc++'s `gate1_`.
    gate1: VecDeque<CtxId>,
    /// The writer that has entered, waiting for the readers to leave:
    /// `gate2_`.
    gate2: Option<CtxId>,
}

/// `Std.BaseSharedMutex` (`lean_io_basesharedmutex_*`).
#[derive(Default)]
pub struct SharedMutex {
    st: RefCell<SharedState>,
}

impl SharedMutex {
    /// `lean_io_basesharedmutex_new`.
    pub fn new() -> SharedMutex {
        super::ensure_started();
        SharedMutex::default()
    }

    /// `lean_io_basesharedmutex_write`.
    pub fn write(&self) {
        super::ensure_started();
        super::writers_point();
        loop {
            {
                let mut m = self.st.borrow_mut();
                if !m.write_entered {
                    m.write_entered = true;
                    break;
                }
                m.gate1.push_back(current_context());
            }
            block_sync();
        }
        loop {
            {
                let mut m = self.st.borrow_mut();
                if m.readers == 0 {
                    return;
                }
                m.gate2 = Some(current_context());
            }
            block_sync();
        }
    }

    /// `lean_io_basesharedmutex_try_write`.
    pub fn try_write(&self) -> bool {
        super::ensure_started();
        let mut m = self.st.borrow_mut();
        if !m.write_entered && m.readers == 0 {
            m.write_entered = true;
            true
        } else {
            false
        }
    }

    /// `lean_io_basesharedmutex_unlock_write`. It switches only to let the
    /// context's handed-off streams end first (`writers_point`).
    pub fn unlock_write(&self) {
        super::ensure_started();
        super::writers_point();
        let ws = {
            let mut m = self.st.borrow_mut();
            m.write_entered = false;
            m.readers = 0;
            std::mem::take(&mut m.gate1)
        };
        for c in ws {
            wake(c);
        }
    }

    /// `lean_io_basesharedmutex_read`.
    pub fn read(&self) {
        super::ensure_started();
        super::writers_point();
        loop {
            {
                let mut m = self.st.borrow_mut();
                if !m.write_entered && m.readers != u32::MAX {
                    m.readers += 1;
                    return;
                }
                m.gate1.push_back(current_context());
            }
            block_sync();
        }
    }

    /// `lean_io_basesharedmutex_try_read`.
    pub fn try_read(&self) -> bool {
        super::ensure_started();
        let mut m = self.st.borrow_mut();
        if !m.write_entered && m.readers != u32::MAX {
            m.readers += 1;
            true
        } else {
            false
        }
    }

    /// `lean_io_basesharedmutex_unlock_read`. It switches only to let the
    /// context's handed-off streams end first (`writers_point`).
    pub fn unlock_read(&self) {
        super::ensure_started();
        super::writers_point();
        let w = {
            let mut m = self.st.borrow_mut();
            m.readers = m.readers.saturating_sub(1);
            if m.write_entered {
                if m.readers == 0 {
                    m.gate2.take()
                } else {
                    None
                }
            } else if m.readers == u32::MAX - 1 {
                m.gate1.pop_front()
            } else {
                None
            }
        };
        if let Some(c) = w {
            wake(c);
        }
    }
}
