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
//! tasks run meanwhile, as other threads would. The owner of a lock is the
//! thread the running code natively runs on: the emulated OS thread that
//! `IO.getTID` names (`tid_offset`, review AR-37), on the OS thread the
//! scheduler runs on (below). So:
//! - `main` is thread 0;
//! - a pool task runs on its emulated worker's thread, which it holds from
//!   its begin to the end of its run, its waits included: two pool tasks
//!   that run at once never share one (a task run on the stack of a waiter
//!   takes another worker than the waiter's), and pool tasks run one after
//!   the other can, as natively the idle worker takes the next task;
//! - a dedicated task has a new thread, as natively
//!   (`spawn_dedicated_worker`);
//! - a `sync` task runs on the thread below it: a `sync` dependent on the
//!   thread that finished its source or resolved its promise, a dependent
//!   run at once (`FAST`) on its caller's;
//! - the event loop's callbacks run on the loop's one thread, the same for
//!   every loop context (one runs at a time).
//!
//! A task that ends with a lock held leaves it held by its thread, as
//! natively: a later task on the same emulated worker takes a recursive
//! mutex again, any other thread waits (hunt HSG-01). Before HSG-01 the
//! owner was the context and the depth of the innermost task on it
//! (`thread_number`): a dedicated task after a task that ended holding a
//! recursive mutex, at the same depth on the same context, took it again,
//! where natively it is another thread; and a pool task on another context
//! than the earlier one found it held by another thread, where natively the
//! same idle worker locks it again (with `lock`, a hang).
//!
//! As natively (glibc), locking a `BaseMutex` that the same thread holds
//! waits forever, and unlocking one that is not locked by the caller just
//! unlocks it. A released mutex is handed to the thread that has waited
//! longest.
//!
//! The owner also names the OS thread the scheduler runs on (`os_thread`):
//! natively the module initializers run on the process's first thread and
//! `main` on a thread of its own (`lean_run_main`; a translator's
//! `io::startup::run_main`), or on the same thread with
//! `LEAN_MAIN_USE_THREAD=0`, and each OS thread has a scheduler of its own.
//! So a `BaseRecursiveMutex` an initializer keeps locked is `main`'s to lock
//! again only when `main` runs on the initializers' thread; on a thread of
//! its own, `main` waits for it forever, as natively (AR-39, lean2rr's review
//! RS7-02). Before AR-39 the owner held whether the scheduler had started
//! instead, which got both cases wrong: `main` on the initializers' thread,
//! with workers, waited for good, and on a thread of its own with
//! `LEAN_NUM_THREADS=0` it took the lock.
//!
//! The objects are plain values; the glue keeps each in its own handle (an
//! `Rc`, or the translator's external object) and calls these methods with
//! it. Each method starts the scheduler first if [`super::start_lazy`] is
//! waiting for it (`ensure_started`), since a wait needs the scheduler's
//! contexts. A lock's owner does not depend on the start: `main`'s thread is
//! the same OS thread and emulated thread 0 before and after it, so a
//! recursive mutex locked by `main` before its first task and again after it
//! has one owner (lean2rr's review RS4-05). A separate module, so that a
//! translator can admit `Std.Sync` on its own (decisions Q8).

use super::{block_sync, current_context, wake, CtxId};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// The thread that runs now (see the module comment): the OS thread the
/// scheduler runs on, and the emulated OS thread of the running code on it
/// (`tid_offset`: the same numbers as `IO.getTID`'s, each scheduler's own).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Owner {
    os: u64,
    tid: u64,
}

fn me() -> Owner {
    Owner {
        os: os_thread(),
        tid: super::tid_offset(),
    }
}

/// A number naming the calling OS thread, from a process-wide counter at
/// the thread's first call: a thread-local load after that, no allocation
/// (`std::thread::current()` would clone an `Arc`). The local is a constant
/// with no destructor, so it can be read at any time, while the thread's
/// locals are being destroyed included. Numbers are not reused.
fn os_thread() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static OS_THREAD: Cell<u64> = const { Cell::new(0) };
    }
    OS_THREAD.with(|t| match t.get() {
        0 => {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            t.set(n);
            n
        }
        n => n,
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

    /// `lean_io_basemutex_try_lock`. A writers point first, as its blocking
    /// counterpart (review HR-03): the context's handed-off streams end
    /// before it reads the lock's state.
    pub fn try_lock(&self) -> bool {
        super::ensure_started();
        super::writers_point();
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

    /// `lean_io_baserecmutex_try_lock`. A writers point first, as its blocking
    /// counterpart (review HR-03): the context's handed-off streams end
    /// before it reads the lock's state.
    pub fn try_lock(&self) -> bool {
        super::ensure_started();
        super::writers_point();
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

    /// `lean_io_basesharedmutex_try_write`. A writers point first, as its blocking
    /// counterpart (review HR-03): the context's handed-off streams end
    /// before it reads the lock's state.
    pub fn try_write(&self) -> bool {
        super::ensure_started();
        super::writers_point();
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

    /// `lean_io_basesharedmutex_try_read`. A writers point first, as its blocking
    /// counterpart (review HR-03): the context's handed-off streams end
    /// before it reads the lock's state.
    pub fn try_read(&self) -> bool {
        super::ensure_started();
        super::writers_point();
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
