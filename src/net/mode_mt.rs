//! `net`'s mode layer for threads mode (feature `threads`; docs/threads.md,
//! 0.7): what the one core (`tcp.rs`, `udp.rs`, `dns.rs`, `mod.rs`) needs
//! from the scheduler, here on `sched::uv`'s loop thread, as natively on
//! libuv's. Each item has the same name and shape as in `mode_st.rs`, the
//! single-thread scheduler's.
//!
//! - A socket's state is a lock in an `Arc` ([`Cell`], [`Shared`]), so a
//!   socket is `Send + Sync`; the open sockets are the process's
//!   ([`register`]).
//! - libuv's io watcher is a watch of `sched::uv`'s loop ([`watch`]),
//!   `uv__io_feed` its pending queue ([`feed_soon`]); both run on the loop
//!   thread with the loop lock held.
//! - The loop's lock ([`loop_lock`]) is `sched::uv`'s, native's
//!   `event_loop_t` mutex: an extern holds it from its start to its end, and
//!   so does Lean's finalizer of a socket ([`finalizer_lock`], the drop of
//!   its last handle, on any thread). A socket's state lock is taken only
//!   under it, never across translator code, so it is never contended; the
//!   order is the loop lock, then a socket's state, then the registry or the
//!   loop's data.
//! - [`MaybeSend`] is `Send`: the loop thread runs and drops what a pending
//!   operation keeps (a `done` closure, the buffers of a `send`, the array of
//!   a receive).
//! - A DNS lookup's answer comes back from a helper thread through the
//!   loop's async queue (`sched::uv`'s `post`, `uv_async_send`), never
//!   through the loop lock ([`dns_client`]).

use super::dns::Answer;
use super::Ready;
use crate::io::IoError;
use crate::sched::mt::uv;
use crate::sched::Interest;
use rustix::fd::OwnedFd;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub(crate) use crate::sched::mt::uv::{LoopGuard, WatchId};
/// A counted reference: to a socket's handle, its state, its descriptor.
pub(crate) use std::sync::Arc as Shared;

/// The bound on what a pending operation keeps (a `done` closure, the
/// buffers of a `send`, the array of a receive): `Send` in threads mode,
/// since the loop thread runs and drops them; nothing in the single-thread
/// mode. Every `Send` type has it.
pub trait MaybeSend: Send {}
impl<T: ?Sized + Send> MaybeSend for T {}

/// A pending operation's completion: it holds the translator's promise
/// (see the module comment of `net`). The loop thread calls or drops it.
pub type Done<T> = Box<dyn FnOnce(Result<T, IoError>) + Send>;

/// A socket's state: a lock with `RefCell`'s two methods, so the core reads
/// the same in both modes. Taken only under the loop lock (the module
/// comment), so it never waits; a second `borrow` on the same thread while
/// one is held would wait for good (the core never takes one while it holds
/// another, as `RefCell` would panic on a `borrow_mut` there).
pub(crate) struct Cell<T>(Mutex<T>);

impl<T> Cell<T> {
    pub(crate) fn new(v: T) -> Cell<T> {
        Cell(Mutex::new(v))
    }

    pub(crate) fn borrow(&self) -> MutexGuard<'_, T> {
        // no translator code runs under it; a lock poisoned by a panic of
        // the crate is taken all the same, as a `RefCell` would be
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn borrow_mut(&self) -> MutexGuard<'_, T> {
        self.borrow()
    }
}

/// A watch's callback.
pub(crate) type WatchCb = uv::WatchCb;

/// A new watch callback.
pub(crate) fn watch_cb(f: impl Fn(Ready) + Send + Sync + 'static) -> WatchCb {
    Arc::new(f)
}

/// `event_loop_lock`: `sched::uv`'s loop lock, held until the guard goes.
/// When the loop thread holds it, the extern interrupts its iteration and
/// waits for its end, so the callbacks of what became ready meanwhile run
/// before the extern acts, as natively. Recursive: a callback's `sync`
/// dependent that calls an extern on the loop thread goes on at once.
#[inline]
pub(crate) fn loop_lock() -> LoopGuard {
    uv::lock()
}

/// Lean's finalizer of a socket (`lean_uv_tcp_socket_finalizer`,
/// `lean_uv_udp_socket_finalizer`) takes the loop lock before `uv_close`.
#[inline]
pub(crate) fn finalizer_lock() -> LoopGuard {
    uv::lock()
}

/// Threads mode starts eagerly (`sched::start`): nothing to build here.
#[inline]
pub(crate) fn ensure_started() {}

/// `uv__io_start` of a new watcher, with the loop lock held: `cb` runs on
/// the loop thread whenever `fd` is ready for `interest`. `None` if epoll
/// refuses (no memory: libuv's abort; here the handle just waits on).
pub(crate) fn watch(fd: &Shared<OwnedFd>, interest: Interest, cb: WatchCb) -> Option<WatchId> {
    uv::watch(fd.clone(), interest, cb).ok()
}

/// A watcher waits for `interest` from now on (the watch is the handle's:
/// `NOENT` cannot happen).
pub(crate) fn watch_modify(id: WatchId, interest: Interest) {
    let _ = uv::watch_modify(id, interest);
}

/// The watch ends; the loop lets go of its clone of the descriptor at once.
pub(crate) fn unwatch(id: WatchId) {
    uv::unwatch(id);
}

/// `uv__io_feed`: run `cb` on the loop thread in its next iteration
/// (libuv's pending queue), with the loop lock held.
pub(crate) fn feed_soon(cb: impl FnOnce() + Send + 'static) {
    uv::pending(Box::new(cb));
}

/// A socket's number in the process's registry of open sockets: given once,
/// never reused.
pub(crate) type SocketId = u64;

/// A socket's state as the registry holds it.
type Entry = Arc<dyn Any + Send + Sync>;

/// The open sockets of the process (TCP and UDP), by number (see
/// `net::Handle`), and the last number given. Held only around plain data:
/// an entry leaves it before it is dropped.
struct Registry {
    last: SocketId,
    open: Option<HashMap<SocketId, Entry>>,
}

static SOCKETS: Mutex<Registry> = Mutex::new(Registry {
    last: 0,
    open: None,
});

fn sockets() -> MutexGuard<'static, Registry> {
    SOCKETS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A new socket's number.
pub(crate) fn new_id() -> SocketId {
    let mut r = sockets();
    r.last += 1;
    r.last
}

/// Socket `id`'s state in the registry.
pub(crate) fn register<T: Send + 'static>(id: SocketId, state: &Shared<Cell<T>>) {
    let entry: Entry = state.clone();
    sockets()
        .open
        .get_or_insert_with(HashMap::new)
        .insert(id, entry);
}

/// Remove socket `id`'s entry; the entry is dropped after the registry's
/// lock.
pub(crate) fn unregister(id: SocketId) {
    let entry = sockets().open.as_mut().and_then(|m| m.remove(&id));
    drop(entry);
}

/// Socket `id`'s state, if it is still open.
pub(crate) fn lookup<T: Send + 'static>(id: SocketId) -> Option<Shared<Cell<T>>> {
    let entry = sockets().open.as_ref().and_then(|m| m.get(&id).cloned());
    entry.and_then(|e| e.downcast::<Cell<T>>().ok())
}

// ---------------------------------------------------------------------------
// DNS: the loop's side of the lookups

/// A lookup's handler of its answer, run on the loop thread.
pub(crate) type DnsDone = Box<dyn FnOnce(Answer) + Send>;

/// The handlers of the lookups in progress, by id, and the last id. Never
/// dropped: a handler whose lookup is in progress when the process exits
/// stays here, its promise unresolved, as natively.
static PENDING: Mutex<(u64, Option<HashMap<u64, DnsDone>>)> = Mutex::new((0, None));

fn pending() -> MutexGuard<'static, (u64, Option<HashMap<u64, DnsDone>>)> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A lookup about to start: `done` is kept here, and the returned closure
/// (a helper's, plain data only) posts the answer to the loop thread
/// (`uv__work_done`), which runs `done` there with the loop lock held. The
/// helper never takes the loop lock: one waiting for it while the loop
/// thread runs a callback that ends the process would never be joined by
/// the exit (`dns::exit_wait`).
///
/// An answer is never delivered after the task manager's finalization
/// (LB-27): the loop thread runs no callback then (`sched::uv`'s module
/// comment), and an answer whose callback still runs is dropped, its `done`
/// neither called nor dropped (its promise stays unresolved, as the
/// single-thread mode's `Client` leaves it at the thread's end).
pub(crate) fn dns_client(done: DnsDone) -> Result<Box<dyn FnOnce(Answer) + Send>, IoError> {
    let id = {
        let mut p = pending();
        p.0 += 1;
        let id = p.0;
        p.1.get_or_insert_with(HashMap::new).insert(id, done);
        id
    };
    Ok(Box::new(move |answer| {
        uv::post(Box::new(move || {
            if crate::sched::mt::manager_finished() {
                return;
            }
            let done = pending().1.as_mut().and_then(|m| m.remove(&id));
            if let Some(done) = done {
                done(answer);
            }
        }));
    }))
}
