//! `net`'s mode layer for the single-thread scheduler (feature `sched`):
//! what the one core (`tcp.rs`, `udp.rs`, `dns.rs`, `mod.rs`) needs from
//! the scheduler, here on the scheduler's one thread (docs/threads.md,
//! 0.7; `mode_mt.rs` is threads mode's). Each item has the same name and
//! shape in both files.
//!
//! - A socket's state is a `RefCell` in an `Rc` ([`Cell`], [`Shared`]),
//!   and the open sockets are the thread's ([`register`]).
//! - libuv's io watcher is a `sched::watch` of the descriptor ([`watch`]),
//!   `uv__io_feed` a timer of the loop due now ([`feed_soon`]); both run on
//!   the scheduler's loop context.
//! - The loop's lock ([`loop_lock`]) lets the loop context run what is due
//!   first (`sched::catch_up`, as `sched::uv`'s externs do) and holds
//!   nothing: one thread runs everything. Lean's finalizer of a socket
//!   takes nothing ([`finalizer_lock`]).
//! - [`MaybeSend`] asks nothing: no value crosses threads.
//! - A DNS lookup's answer comes back from a helper thread through a
//!   channel and the loop's eventfd, which the loop context watches while a
//!   lookup is pending ([`dns_client`]).

use super::dns::Answer;
use super::Ready;
use crate::io::IoError;
use crate::sched::{self, Interest};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;
use std::time::Instant;

pub(crate) use crate::sched::WatchId;
/// A socket's state.
pub(crate) use std::cell::RefCell as Cell;
/// A counted reference: to a socket's handle, its state, its descriptor.
pub(crate) use std::rc::Rc as Shared;

/// The bound on what a pending operation keeps (a `done` closure, the
/// buffers of a `send`, the array of a receive) beyond what one thread
/// needs: none in the single-thread mode. In threads mode it is `Send`, since
/// the loop thread runs and drops them. Every type has it here.
pub trait MaybeSend {}
impl<T: ?Sized> MaybeSend for T {}

/// A pending operation's completion: it holds the translator's promise
/// (see the module comment of `net`).
pub type Done<T> = Box<dyn FnOnce(Result<T, IoError>)>;

/// A watch's callback.
pub(crate) type WatchCb = Rc<dyn Fn(Ready)>;

/// A new watch callback.
pub(crate) fn watch_cb(f: impl Fn(Ready) + 'static) -> WatchCb {
    Rc::new(f)
}

/// The loop's lock, held until the guard goes: here nothing is held.
pub(crate) struct LoopGuard;

/// `event_loop_lock`: natively an extern takes the loop's lock, which makes
/// the loop thread finish its iteration first, so the callbacks of what
/// became ready meanwhile run before the extern acts. Here the loop context
/// runs what is due (`sched::catch_up`, as `sched::uv`'s externs do). Called
/// with no socket borrowed: the callbacks may use the socket.
#[inline]
pub(crate) fn loop_lock() -> LoopGuard {
    sched::catch_up();
    LoopGuard
}

/// The loop's lock in Lean's finalizer of a socket (the drop of its last
/// handle): nothing to take, and no loop run there.
#[inline]
pub(crate) fn finalizer_lock() -> LoopGuard {
    LoopGuard
}

/// The lazy start of the scheduler (`sched::start_lazy`): an extern that
/// makes a socket or starts a lookup builds the scheduler first.
#[inline]
pub(crate) fn ensure_started() {
    sched::ensure_started();
}

/// `uv__io_start` of a new watcher: `cb` runs on the loop context whenever
/// `fd` is ready for `interest`. `None` if epoll refuses (no memory: libuv's
/// abort; here the handle just waits on).
pub(crate) fn watch(fd: &Shared<OwnedFd>, interest: Interest, cb: WatchCb) -> Option<WatchId> {
    sched::watch(fd.clone(), interest, cb).ok()
}

/// A watcher waits for `interest` from now on (the watch is the handle's:
/// `NOENT` cannot happen).
pub(crate) fn watch_modify(id: WatchId, interest: Interest) {
    let _ = sched::watch_modify(id, interest);
}

/// The watch ends; the loop lets go of its clone of the descriptor at once.
pub(crate) fn unwatch(id: WatchId) {
    sched::unwatch(id);
}

/// `uv__io_feed`: run `cb` on the loop context soon, as a timer due now.
pub(crate) fn feed_soon(cb: impl Fn() + 'static) {
    sched::timer_start(Instant::now(), Rc::new(cb));
}

/// A socket's number in the thread's registry of open sockets: given once,
/// never reused.
pub(crate) type SocketId = u64;

/// The open sockets of the thread (TCP and UDP), by number (see
/// `net::Handle`).
#[derive(Default)]
struct Registry {
    /// The last number given.
    last: std::cell::Cell<SocketId>,
    open: RefCell<HashMap<SocketId, Rc<dyn Any>>>,
}

thread_local! {
    static SOCKETS: Registry = Registry::default();
}

/// A new socket's number.
pub(crate) fn new_id() -> SocketId {
    SOCKETS.with(|r| {
        let id = r.last.get() + 1;
        r.last.set(id);
        id
    })
}

/// Socket `id`'s state in the registry.
pub(crate) fn register<T: 'static>(id: SocketId, state: &Shared<Cell<T>>) {
    let entry: Rc<dyn Any> = state.clone();
    SOCKETS.with(|r| r.open.borrow_mut().insert(id, entry));
}

/// Remove socket `id`'s entry (nothing once the thread's locals are gone);
/// the entry is dropped after the registry's borrow.
pub(crate) fn unregister(id: SocketId) {
    let entry = SOCKETS
        .try_with(|r| r.open.borrow_mut().remove(&id))
        .ok()
        .flatten();
    drop(entry);
}

/// Socket `id`'s state, if it is still open.
pub(crate) fn lookup<T: 'static>(id: SocketId) -> Option<Shared<Cell<T>>> {
    SOCKETS
        .try_with(|r| r.open.borrow().get(&id).cloned())
        .ok()
        .flatten()
        .and_then(|e| e.downcast::<RefCell<T>>().ok())
}

// ---------------------------------------------------------------------------
// DNS: the loop's side of the lookups

/// A lookup's handler of its answer, run on the loop context.
pub(crate) type DnsDone = Box<dyn FnOnce(Answer)>;

/// A lookup's id and answer.
type Reply = (u64, Answer);

/// The loop's side: the handlers of the lookups in progress, and the watch
/// of the wake-up descriptor while there are any.
#[derive(Default)]
struct Client {
    next: u64,
    pending: HashMap<u64, DnsDone>,
    replies: Option<(Sender<Reply>, Receiver<Reply>)>,
    watch: Option<sched::WatchId>,
}

impl Drop for Client {
    fn drop(&mut self) {
        // at thread exit: the promises are not dropped (their destructors
        // would call into the scheduler during its destruction)
        for (_, done) in self.pending.drain() {
            std::mem::forget(done);
        }
    }
}

thread_local! {
    static CLIENT: RefCell<Client> = RefCell::new(Client::default());
}

/// The loop's wake-up descriptor: native's async eventfd (opened by the
/// glue at startup), or one of the crate's own.
fn wake_fd() -> Result<BorrowedFd<'static>, IoError> {
    if let Some(fd) = crate::io::startup::loop_eventfd() {
        return Ok(fd);
    }
    // made once; a failure is not kept, so a later lookup tries again
    // (review RNET-05)
    static OWN: OnceLock<OwnedFd> = OnceLock::new();
    if let Some(fd) = OWN.get() {
        return Ok(fd.as_fd());
    }
    let fd = rustix::event::eventfd(
        0,
        rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
    )
    .map_err(|e| IoError::decode_io_error(e.raw_os_error(), None))?;
    Ok(OWN.get_or_init(|| fd).as_fd())
}

/// A lookup about to start: `done` is kept on the loop's side, and the
/// returned closure (a helper's, plain data only) sends the answer back and
/// wakes the loop, whose watch of the wake-up descriptor runs `done` on the
/// loop context.
pub(crate) fn dns_client(done: DnsDone) -> Result<Box<dyn FnOnce(Answer) + Send>, IoError> {
    let wake = wake_fd()?;
    let (id, reply, watch) = CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        c.next += 1;
        let id = c.next;
        c.pending.insert(id, done);
        let reply = c.replies.get_or_insert_with(channel).0.clone();
        (id, reply, c.watch.is_none())
    });
    if watch {
        let cb: Rc<dyn Fn(Ready)> = Rc::new(move |_| deliver(wake));
        let w = sched::watch(wake, Interest::READ, cb).ok();
        CLIENT.with(|c| c.borrow_mut().watch = w);
    }
    Ok(Box::new(move |answer| {
        if reply.send((id, answer)).is_ok() {
            // `uv_async_send`: wake the loop
            let _ = rustix::io::write(wake, &1u64.to_ne_bytes());
        }
    }))
}

/// The wake-up descriptor is readable: run the handlers of the answered
/// lookups, in the order the helpers answered; stop watching once none is
/// left.
fn deliver(wake: BorrowedFd<'static>) {
    let mut buf = [0u8; 8];
    let _ = rustix::io::read(wake, &mut buf);
    loop {
        let next = CLIENT.with(|c| {
            let mut c = c.borrow_mut();
            let (id, answer) = c.replies.as_ref()?.1.try_recv().ok()?;
            let done = c.pending.remove(&id)?;
            Some((done, answer))
        });
        let Some((done, answer)) = next else {
            break;
        };
        done(answer);
    }
    let w = CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        if c.pending.is_empty() {
            c.watch.take()
        } else {
            None
        }
    });
    if let Some(w) = w {
        sched::unwatch(w);
    }
}
