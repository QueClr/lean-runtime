//! `Std.Sync`'s primitives in threads mode (`src/runtime/mutex.cpp`):
//! `BaseMutex` (`std::mutex`), `Condvar` (`std::condition_variable`),
//! `BaseRecursiveMutex` (`std::recursive_mutex`) and `BaseSharedMutex`
//! (libc++'s `std::shared_mutex`), with the single-thread scheduler's
//! methods (`src/sched/sync.rs`). `Std.Channel`, `Std.Barrier`,
//! `Std.Semaphore` and the rest of `Std.Sync` are Lean code over these and
//! `IO.Promise`.
//!
//! A thread that must wait blocks on the object's condition variable. Each
//! object keeps its state under a lock of its own (never held together with
//! another object's, a `Ref`'s or the scheduler's), so every method may be
//! called from any thread. The rules are sched-1's:
//! - the owner of a lock is a thread: the OS thread, and whether the task
//!   manager ran when it locked (natively the module initializers run on
//!   the process's main thread, and `main` on a thread of its own,
//!   `lean_run_main`);
//! - locking a `BaseMutex` that the same thread holds waits forever, as
//!   glibc's does; unlocking one that the caller does not hold just unlocks
//!   it;
//! - a released mutex (plain or recursive) is handed to the thread that has
//!   waited longest: one of native's outcomes (glibc's mutex promises no
//!   order);
//! - a `Condvar` wait returns only once notified (native's may also wake
//!   spuriously: both are native outcomes).
//!
//! The shared mutex is libc++'s `__shared_mutex_base` (Lean's runtime is
//! built with libc++), with its two condition variables: a writer that has
//! entered keeps new readers out and waits for the readers inside to leave.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar as StdCondvar, Mutex as StdMutex, MutexGuard, PoisonError};

/// The thread that runs now (see the module comment).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Owner {
    thread: u64,
    started: bool,
}

/// A number for each OS thread, from a process-wide counter (0 while the
/// thread's locals are being destroyed).
fn thread_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static TAG: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    TAG.try_with(|t| *t).unwrap_or(0)
}

fn me() -> Owner {
    Owner {
        thread: thread_tag(),
        started: super::manager_running(),
    }
}

fn lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wait<'a, T>(cv: &StdCondvar, g: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    cv.wait(g).unwrap_or_else(PoisonError::into_inner)
}

/// The waiters of a lock that hands itself over: tickets in arrival order,
/// and the tickets an unlock has granted (each waiter takes its own).
#[derive(Default)]
struct Handover {
    next: u64,
    queue: VecDeque<(u64, Owner)>,
    granted: Vec<u64>,
}

impl Handover {
    /// Join the queue as `who`: the ticket to wait for.
    fn enqueue(&mut self, who: Owner) -> u64 {
        let t = self.next;
        self.next = self.next.wrapping_add(1);
        self.queue.push_back((t, who));
        t
    }

    /// Whether ticket `t` was granted (and take the grant).
    fn take_grant(&mut self, t: u64) -> bool {
        match self.granted.iter().position(|&g| g == t) {
            Some(k) => {
                self.granted.swap_remove(k);
                true
            }
            None => false,
        }
    }

    /// Hand the lock to the longest waiter: its owner, or `None` if none
    /// waits. The caller notifies the condition variable.
    fn hand(&mut self) -> Option<Owner> {
        let (t, who) = self.queue.pop_front()?;
        self.granted.push(t);
        Some(who)
    }
}

// ---------------------------------------------------------------------------
// BaseMutex

#[derive(Default)]
struct MutexState {
    owner: Option<Owner>,
    waiters: Handover,
}

/// `Std.BaseMutex` (`lean_io_basemutex_*`).
#[derive(Default)]
pub struct Mutex {
    st: StdMutex<MutexState>,
    /// A waiter's ticket was granted.
    handed: StdCondvar,
}

impl Mutex {
    /// `lean_io_basemutex_new`.
    pub fn new() -> Mutex {
        Mutex::default()
    }

    fn lock_as(&self, who: Owner) {
        let mut m = lock(&self.st);
        if m.owner.is_none() {
            m.owner = Some(who);
            return;
        }
        // Held, by another thread or (a deadlock, as natively) by this one:
        // `unlock` hands it over.
        let t = m.waiters.enqueue(who);
        while !m.waiters.take_grant(t) {
            m = wait(&self.handed, m);
        }
    }

    /// `lean_io_basemutex_lock`.
    pub fn lock(&self) {
        self.lock_as(me());
    }

    /// `lean_io_basemutex_try_lock`.
    pub fn try_lock(&self) -> bool {
        let who = me();
        let mut m = lock(&self.st);
        if m.owner.is_none() {
            m.owner = Some(who);
            true
        } else {
            false
        }
    }

    /// `lean_io_basemutex_unlock`.
    pub fn unlock(&self) {
        let mut m = lock(&self.st);
        m.owner = m.waiters.hand();
        let handed = m.owner.is_some();
        drop(m);
        if handed {
            self.handed.notify_all();
        }
    }
}

// ---------------------------------------------------------------------------
// Condvar

#[derive(Default)]
struct CondState {
    next: u64,
    /// The waiters, in arrival order.
    waiting: VecDeque<u64>,
    /// The waiters notified, not woken yet.
    notified: Vec<u64>,
}

/// `Std.Condvar` (`lean_io_condvar_*`).
#[derive(Default)]
pub struct Condvar {
    st: StdMutex<CondState>,
    cv: StdCondvar,
}

impl Condvar {
    /// `lean_io_condvar_new`.
    pub fn new() -> Condvar {
        Condvar::default()
    }

    /// `lean_io_condvar_wait`: release `m`, wait to be notified, then take
    /// `m` again (natively `condition_variable::wait` on the adopted lock).
    /// The thread is a waiter before it releases `m`, so a notification by
    /// whoever takes `m` next reaches it.
    pub fn wait(&self, m: &Mutex) {
        let who = me();
        let t = {
            let mut c = lock(&self.st);
            let t = c.next;
            c.next = c.next.wrapping_add(1);
            c.waiting.push_back(t);
            t
        };
        m.unlock();
        {
            let mut c = lock(&self.st);
            loop {
                if let Some(k) = c.notified.iter().position(|&n| n == t) {
                    c.notified.swap_remove(k);
                    break;
                }
                c = wait(&self.cv, c);
            }
        }
        m.lock_as(who);
    }

    /// `lean_io_condvar_notify_one`: the longest waiter.
    pub fn notify_one(&self) {
        let mut c = lock(&self.st);
        if let Some(t) = c.waiting.pop_front() {
            c.notified.push(t);
            drop(c);
            self.cv.notify_all();
        }
    }

    /// `lean_io_condvar_notify_all`.
    pub fn notify_all(&self) {
        let mut c = lock(&self.st);
        if c.waiting.is_empty() {
            return;
        }
        let ws: Vec<u64> = c.waiting.drain(..).collect();
        c.notified.extend(ws);
        drop(c);
        self.cv.notify_all();
    }
}

// ---------------------------------------------------------------------------
// BaseRecursiveMutex

#[derive(Default)]
struct RecState {
    owner: Option<Owner>,
    count: u32,
    waiters: Handover,
}

/// `Std.BaseRecursiveMutex` (`lean_io_baserecmutex_*`).
#[derive(Default)]
pub struct RecursiveMutex {
    st: StdMutex<RecState>,
    handed: StdCondvar,
}

impl RecursiveMutex {
    /// `lean_io_baserecmutex_new`.
    pub fn new() -> RecursiveMutex {
        RecursiveMutex::default()
    }

    /// `lean_io_baserecmutex_lock`.
    pub fn lock(&self) {
        let who = me();
        let mut m = lock(&self.st);
        match m.owner {
            None => {
                m.owner = Some(who);
                m.count = 1;
            }
            Some(o) if o == who => m.count += 1,
            Some(_) => {
                let t = m.waiters.enqueue(who);
                while !m.waiters.take_grant(t) {
                    m = wait(&self.handed, m);
                }
            }
        }
    }

    /// `lean_io_baserecmutex_try_lock`.
    pub fn try_lock(&self) -> bool {
        let who = me();
        let mut m = lock(&self.st);
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

    /// `lean_io_baserecmutex_unlock`.
    pub fn unlock(&self) {
        let mut m = lock(&self.st);
        if m.count > 1 {
            m.count -= 1;
            return;
        }
        m.owner = m.waiters.hand();
        m.count = u32::from(m.owner.is_some());
        let handed = m.owner.is_some();
        drop(m);
        if handed {
            self.handed.notify_all();
        }
    }
}

// ---------------------------------------------------------------------------
// BaseSharedMutex: libc++'s `__shared_mutex_base`

#[derive(Default)]
struct SharedState {
    write_entered: bool,
    readers: u32,
}

/// `Std.BaseSharedMutex` (`lean_io_basesharedmutex_*`).
#[derive(Default)]
pub struct SharedMutex {
    st: StdMutex<SharedState>,
    /// Waiting to enter, writers and readers: libc++'s `gate1_`.
    gate1: StdCondvar,
    /// The writer that has entered, waiting for the readers to leave:
    /// `gate2_`.
    gate2: StdCondvar,
}

impl SharedMutex {
    /// `lean_io_basesharedmutex_new`.
    pub fn new() -> SharedMutex {
        SharedMutex::default()
    }

    /// `lean_io_basesharedmutex_write` (`lock`).
    pub fn write(&self) {
        let mut m = lock(&self.st);
        while m.write_entered {
            m = wait(&self.gate1, m);
        }
        m.write_entered = true;
        while m.readers > 0 {
            m = wait(&self.gate2, m);
        }
    }

    /// `lean_io_basesharedmutex_try_write` (`try_lock`).
    pub fn try_write(&self) -> bool {
        let mut m = lock(&self.st);
        if !m.write_entered && m.readers == 0 {
            m.write_entered = true;
            true
        } else {
            false
        }
    }

    /// `lean_io_basesharedmutex_unlock_write` (`unlock`).
    pub fn unlock_write(&self) {
        let mut m = lock(&self.st);
        m.write_entered = false;
        m.readers = 0;
        drop(m);
        self.gate1.notify_all();
    }

    /// `lean_io_basesharedmutex_read` (`lock_shared`).
    pub fn read(&self) {
        let mut m = lock(&self.st);
        while m.write_entered || m.readers == u32::MAX {
            m = wait(&self.gate1, m);
        }
        m.readers += 1;
    }

    /// `lean_io_basesharedmutex_try_read` (`try_lock_shared`).
    pub fn try_read(&self) -> bool {
        let mut m = lock(&self.st);
        if !m.write_entered && m.readers != u32::MAX {
            m.readers += 1;
            true
        } else {
            false
        }
    }

    /// `lean_io_basesharedmutex_unlock_read` (`unlock_shared`).
    pub fn unlock_read(&self) {
        let mut m = lock(&self.st);
        m.readers = m.readers.saturating_sub(1);
        if m.write_entered {
            if m.readers == 0 {
                drop(m);
                self.gate2.notify_one();
            }
        } else if m.readers == u32::MAX - 1 {
            drop(m);
            self.gate1.notify_one();
        }
    }
}
