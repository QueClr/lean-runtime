//! Networking (feature `net`): Lean 4.34.0's TCP, UDP, DNS and interface
//! externs (`src/runtime/uv/tcp.cpp`, `udp.cpp`, `dns.cpp`, `net_addr.cpp`,
//! over libuv 1.48), on the scheduler's event loop: the single-thread
//! scheduler's (feature `sched`) or, in threads mode (feature `threads`),
//! `sched::uv`'s loop thread, as natively (docs/threads.md, 0.7).
//!
//! # Layout
//!
//! | Module | What |
//! |---|---|
//! | [`tcp`] | `Std.Internal.UV.TCP.Socket`: libuv's stream model (`uv_tcp_t`) over a non-blocking socket |
//! | [`udp`] | `Std.Internal.UV.UDP.Socket`: libuv's `uv_udp_t` |
//! | [`dns`] | `Std.Internal.UV.DNS`: glibc's `getaddrinfo` and `getnameinfo` with libuv's hints, on helper threads |
//! | [`iface`] | `Std.Net.interfaceAddresses` (`uv_interface_addresses`) |
//!
//! # The model
//!
//! Natively a dedicated thread runs libuv's loop, and the externs lock it to
//! start an operation; the loop thread later resolves the operation's Lean
//! promise. Here each socket mirrors libuv's handle: the same flags, queues
//! and system calls in the same order, made by the extern itself where libuv
//! makes them in the calling thread (a `send` writes at once what fits, a
//! `bind` binds), and by a callback on the scheduler's loop context where
//! libuv's loop thread makes them (`sched::watch` for the socket's
//! descriptor, a due timer for libuv's `uv__io_feed`). The callbacks resolve
//! the promises there, so their waiters wake and their `sync` dependents run
//! on the loop context, as natively on the loop thread.
//!
//! **Promises.** An operation that returns a promise takes `done`, a
//! one-shot closure holding the translator's promise (natively the loop's
//! `lean_inc(promise)`): calling it resolves the promise with the result,
//! dropping it uncalled is `lean_dec` (`cancelRecv`, `cancelAccept`), after
//! which the promise resolves to `none` only when the program drops its own
//! last reference, as natively. `done` may run before the extern returns
//! where native resolves the promise before returning (an empty `send`, an
//! `accept` of a connection the loop already took).
//!
//! **Both modes, one core.** This module, `tcp`, `udp` and `dns` are one
//! source for both schedulers; `mode` (`mode_st.rs` or `mode_mt.rs`) gives
//! what differs: the socket's cell and counted reference (`RefCell` in an
//! `Rc`, or a lock in an `Arc`), the registry of open sockets (the thread's
//! or the process's), the io watcher and `uv__io_feed` (the scheduler's
//! loop context, or the loop thread), the loop's lock (`sched::catch_up`,
//! or `sched::uv`'s lock held through the extern), and [`MaybeSend`], the
//! bound on what a pending operation keeps (none, or `Send`). In threads
//! mode the loop thread runs the callbacks with the loop lock held, a
//! socket's state lock is taken only under it, and Lean's finalizer of a
//! socket (the drop of its last handle, on any thread) takes it too.
//!
//! **Lifetime.** A socket closes when the translator's last handle goes
//! (Lean's finalizer, `uv_close`), unless an operation is pending: a pending
//! operation holds the socket, as native's `lean_inc(socket)`. The loop's
//! callbacks (the watch of the descriptor, a due `uv__io_feed`) hold no
//! reference to the socket, only its number in the registry of open sockets
//! ([`Handle`]); one that runs after the socket closed finds nothing
//! and does nothing. So only the program's handles and the pending
//! operations keep a socket open, and no reference cycle goes through the
//! loop.
//!
//! **Data.** `send` takes the translator's buffers as a [`SendData`] view and
//! keeps it until the write completes (Lean's `Array ByteArray` is held
//! until the callback); `recv` reads into the translator's own new
//! `ByteArray` through [`RecvBuf`], allocated by a closure the crate calls
//! where native allocates it, so neither side copies bulk data.
//!
//! **Errors** are [`IoError`]s built as Lean's externs build them
//! (`lean_decode_uv_error(code, nullptr)` for libuv's codes). Internally a
//! failure is libuv's code: a negative `errno`, or one of the `UV_*` codes
//! below.
//!
//! Native bugs this module does not reproduce (`docs/lean-bugs.md`) are
//! marked `LEAN-BUG:` at the line that differs.
//!
//! See `docs/net.md` for the design, the cases and the differences.

use crate::io::IoError;
use crate::sched::{Interest, Ready};
use mode::{Cell, Shared, SocketId, WatchId};
use rustix::fd::{BorrowedFd, OwnedFd};
use rustix::net::SocketAddrAny;
use std::mem::MaybeUninit;

pub mod dns;
pub mod iface;
// The scheduler's side, one per mode (docs/threads.md, 0.7). With both
// schedulers on, `sched` is the single-thread one (src/lib.rs), so its side
// is taken and only src/lib.rs's error is reported.
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
#[path = "mode_st.rs"]
mod mode;
#[cfg(all(feature = "threads", not(feature = "sched")))]
#[path = "mode_mt.rs"]
mod mode;
pub mod tcp;
pub mod udp;

#[cfg(test)]
mod tests;
#[cfg(all(test, feature = "threads", not(feature = "sched")))]
mod tests_mt;

pub use mode::{Done, MaybeSend};
pub use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

// libuv's error codes on Linux (`uv-errno.h`): the negated `errno`, and
// libuv's own codes.
pub(crate) const UV_EBADF: i32 = -9;
pub(crate) const UV_EAGAIN: i32 = -11;
pub(crate) const UV_EINVAL: i32 = -22;
pub(crate) const UV_EPIPE: i32 = -32;
pub(crate) const UV_EDESTADDRREQ: i32 = -89;
pub(crate) const UV_EAFNOSUPPORT: i32 = -97;
pub(crate) const UV_EADDRINUSE: i32 = -98;
pub(crate) const UV_ENOBUFS: i32 = -105;
pub(crate) const UV_EISCONN: i32 = -106;
pub(crate) const UV_ENOTCONN: i32 = -107;
pub(crate) const UV_ECONNREFUSED: i32 = -111;
pub(crate) const UV_EALREADY: i32 = -114;
pub(crate) const UV_EINPROGRESS: i32 = -115;
pub(crate) const UV_ECANCELED: i32 = -125;

/// `event_loop_lock`, held until the guard goes: natively an extern takes
/// the loop's lock, which makes the loop thread finish its iteration first,
/// so the callbacks of what became ready meanwhile run before the extern
/// acts. The single-thread mode lets the loop context run what is due
/// (`sched::catch_up`, as `sched::uv`'s externs do) and holds nothing;
/// threads mode holds `sched::uv`'s loop lock until the guard goes. Called
/// with no socket borrowed: the callbacks may use the socket.
#[inline]
pub(crate) fn loop_lock() -> mode::LoopGuard {
    mode::loop_lock()
}

/// `UV__ERR(errno)`: libuv's code of a failed system call.
#[inline]
pub(crate) fn uv_err(e: rustix::io::Errno) -> i32 {
    -e.raw_os_error()
}

/// `lean_decode_uv_error(code, nullptr)`.
#[inline]
pub(crate) fn uv_error(code: i32) -> IoError {
    IoError::decode_uv_error(code, None)
}

/// `lean_promise_resolve_with_code(status, promise)` (event_loop.cpp): `ok
/// ()` for 0, else the decoded error.
#[inline]
pub(crate) fn with_code(status: i32) -> Result<(), IoError> {
    if status == 0 {
        Ok(())
    } else {
        Err(uv_error(status))
    }
}

/// `IOV_MAX` (`uv__getiovmax`).
pub(crate) const IOV_MAX: usize = 1024;

/// The events libuv hands an io watcher's callback (`POLLIN`, `POLLOUT`,
/// `POLLHUP`), from what the scheduler's loop saw: `read` is `POLLIN`,
/// `POLLERR` or `POLLHUP`, as libuv's stream code tests them together;
/// `write` is `POLLOUT`, `POLLERR` or `POLLHUP`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ev {
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) hup: bool,
}

impl Ev {
    /// `uv__io_feed`'s call: `POLLOUT` alone.
    pub(crate) const FEED: Ev = Ev {
        read: false,
        write: true,
        hup: false,
    };

    /// What libuv's `uv__io_poll` hands the callback of a watcher that waits
    /// for `want`: the events epoll reported among those (an error or a
    /// hang-up always counts).
    pub(crate) fn from_ready(r: Ready, want: Interest) -> Ev {
        let err = r.error || r.hangup;
        Ev {
            read: (want.read && r.read) || err,
            write: (want.write && r.write) || err,
            hup: r.hangup,
        }
    }

    /// libuv's `uv__io_poll` for a watcher whose callback tests `POLLIN` and
    /// `POLLOUT` alone (a UDP handle), from the raw events: they are kept to
    /// what the watcher waits for plus `POLLERR` and `POLLHUP`, and when
    /// exactly `POLLERR` or exactly `POLLHUP` is left, the watched events
    /// are added (libuv's work-around of an epoll quirk, `linux.c`). So an
    /// error with `POLLOUT` sends but does not receive, and `POLLERR` with
    /// `POLLHUP` does neither.
    pub(crate) fn libuv_merge(
        raw_in: bool,
        raw_out: bool,
        err: bool,
        hup: bool,
        want: Interest,
    ) -> Ev {
        let mut read = raw_in && want.read;
        let mut write = raw_out && want.write;
        if !read && !write && (err != hup) {
            read = want.read;
            write = want.write;
        }
        Ev { read, write, hup }
    }
}

/// A libuv io watcher (`uv__io_t`): the events its handle waits for
/// (`uv__io_start`, `uv__io_stop`), kept in step with one watch of the
/// descriptor by the loop (`mode::watch`; none while it waits for nothing).
#[derive(Default)]
pub(crate) struct IoWatcher {
    want: Interest,
    id: Option<WatchId>,
    /// A `uv__io_feed` is queued and has not run.
    pub(crate) fed: bool,
}

impl IoWatcher {
    /// Wait for `want` on `fd` (`cb` makes the callback of a new watch,
    /// which holds a clone of `fd` until it ends). Without a descriptor
    /// nothing is watched (libuv's watcher has none either until the socket
    /// exists).
    pub(crate) fn set(
        &mut self,
        fd: Option<&Fd>,
        want: Interest,
        cb: impl FnOnce() -> mode::WatchCb,
    ) {
        let Some(fd) = fd else {
            self.want = want;
            return;
        };
        let empty = !want.read && !want.write;
        match self.id {
            Some(id) if empty => {
                mode::unwatch(id);
                self.id = None;
            }
            Some(id) if want != self.want => {
                mode::watch_modify(id, want);
            }
            None if !empty => {
                self.id = mode::watch(fd, want, cb());
            }
            _ => {}
        }
        self.want = want;
    }

    pub(crate) fn want(&self) -> Interest {
        self.want
    }

    /// `uv__io_close`: stop watching (the loop lets go of its clone of the
    /// descriptor at once).
    pub(crate) fn close(&mut self) {
        if let Some(id) = self.id.take() {
            mode::unwatch(id);
        }
        self.want = Interest::default();
    }
}

#[cfg(test)]
thread_local! {
    /// How many loop callbacks of this thread found their socket (the unit
    /// tests'; in threads mode the loop thread's).
    pub(crate) static CALLBACKS_RUN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// A socket as the program holds it: `TcpSocket` and `UdpSocket` are a
/// counted reference (`mode::Shared`) to one, which a pending operation
/// clones (native's `lean_inc(socket)`). It owns the socket's state and its
/// entry in the registry of open sockets (`mode::register`: the thread's,
/// or in threads mode the process's), by number, a number given once and
/// never reused. The loop's callbacks hold a socket's number, not a
/// reference to it, and find the socket there while it is open
/// ([`on_socket`]). An entry exists exactly while the socket's handle does
/// (made by [`Handle::new`], removed by its drop), and the handle holds the
/// socket's state too: the registry is never the last owner of a socket,
/// so removing an entry, or the registry itself at the thread's end, never
/// closes one.
///
/// When the last clone goes (the program's last reference, and no
/// operation pending), its drop (Lean's finalizer, which takes the loop's
/// lock: `mode::finalizer_lock`) removes the entry and lets go of the
/// state, whose own drop closes the descriptor (`uv_close`): at once,
/// unless a loop callback is running on the socket, which holds the state
/// until it returns ([`on_socket`]).
pub(crate) struct Handle<T: MaybeSend + 'static> {
    id: SocketId,
    /// Always `Some` until the drop, which lets go of it under the loop's
    /// lock.
    state: Option<Shared<Cell<T>>>,
}

impl<T: MaybeSend + 'static> Handle<T> {
    /// A new socket: its number, its state `make(id)`, its entry.
    pub(crate) fn new(make: impl FnOnce(SocketId) -> T) -> Handle<T> {
        let id = mode::new_id();
        let state = Shared::new(Cell::new(make(id)));
        mode::register(id, &state);
        Handle {
            id,
            state: Some(state),
        }
    }
}

/// The socket's state.
impl<T: MaybeSend + 'static> std::ops::Deref for Handle<T> {
    type Target = Cell<T>;
    fn deref(&self) -> &Cell<T> {
        self.state.as_deref().expect("a live handle has its state")
    }
}

impl<T: MaybeSend + 'static> Drop for Handle<T> {
    fn drop(&mut self) {
        let _l = mode::finalizer_lock();
        // nothing to remove once the thread's locals are gone
        mode::unregister(self.id);
        // the socket's drop, if nothing else holds it, under the loop's lock
        drop(self.state.take());
    }
}

/// A loop callback's step: run `f` on socket `id` if it is still open;
/// otherwise do nothing. `f` holds the socket's state until it returns: a
/// socket whose last handle goes during the call (its last operation
/// resolved there) closes when `f` returns. Natively Lean's finalizer takes
/// the loop's lock (`event_loop_lock`, recursive) before `uv_close`, so its
/// close too comes no later than the end of the callback; the rest of the
/// call does nothing visible, since no operation is pending any more. A
/// watch's callback always finds its socket (the socket's drop ends the
/// watch, `IoWatcher::close`, before any other callback can run); a
/// `uv__io_feed` due after the socket closed does not.
pub(crate) fn on_socket<T: MaybeSend + 'static>(id: SocketId, f: impl FnOnce(&Cell<T>)) {
    let Some(s) = mode::lookup::<T>(id) else {
        return;
    };
    #[cfg(test)]
    CALLBACKS_RUN.with(|n| n.set(n.get() + 1));
    f(&s);
}

/// `uv__io_feed`: run `io(FEED)` on the loop soon (once until it has run),
/// as libuv's pending queue runs the watcher with `POLLOUT` at its next
/// turn. The callback holds the socket's number only.
pub(crate) fn feed<T: MaybeSend + 'static>(
    w: &mut IoWatcher,
    id: SocketId,
    io: fn(&Cell<T>, Ev),
    fed: fn(&mut T) -> &mut IoWatcher,
) {
    if w.fed {
        return;
    }
    w.fed = true;
    mode::feed_soon(move || {
        on_socket(id, |s: &Cell<T>| {
            fed(&mut s.borrow_mut()).fed = false;
            io(s, Ev::FEED);
        })
    });
}

/// The watch callback of socket `id`: `io(events)` while the socket is
/// open. It holds the socket's number only.
pub(crate) fn watch_cb<T: MaybeSend + 'static>(
    id: SocketId,
    io: fn(&Cell<T>, Ev),
    want: fn(&T) -> Interest,
    conv: fn(Ready, Interest) -> Ev,
) -> mode::WatchCb {
    mode::watch_cb(move |r| {
        on_socket(id, |s: &Cell<T>| {
            let w = want(&s.borrow());
            io(s, conv(r, w));
        })
    })
}

/// A Lean `SocketAddress` as a socket address for the system calls
/// (`lean_socket_address_to_sockaddr_storage`: no flow label, no scope).
pub(crate) fn sockaddr(a: &SocketAddr) -> SocketAddrAny {
    match a {
        SocketAddr::V4(v4) => SocketAddrAny::from(SocketAddrV4::new(*v4.ip(), v4.port())),
        SocketAddr::V6(v6) => SocketAddrAny::from(SocketAddrV6::new(*v6.ip(), v6.port(), 0, 0)),
    }
}

/// `lean_sockaddr_to_socketaddress`: an IPv4 or IPv6 address (native has no
/// other family here; it would be `lean_unreachable`).
pub(crate) fn socket_address(a: SocketAddrAny) -> Option<SocketAddr> {
    SocketAddr::try_from(a).ok()
}

/// The translator's buffers of a `send` (Lean's `Array ByteArray`), kept by
/// the crate until the write completes (in threads mode the loop thread
/// writes and drops them: [`MaybeSend`] is `Send` there). Its methods run
/// while the crate holds the socket's state: they must only read the bytes,
/// not call into the crate or yield (no effect point, no wait).
pub trait SendData: MaybeSend + 'static {
    /// The number of buffers.
    fn count(&self) -> usize;
    /// The bytes of buffer `i`.
    fn get(&self, i: usize) -> &[u8];
}

impl SendData for Vec<Vec<u8>> {
    fn count(&self) -> usize {
        self.len()
    }
    fn get(&self, i: usize) -> &[u8] {
        &self[i]
    }
}

/// Where a receive puts its bytes: the translator's new `ByteArray` of the
/// size asked for (Lean's `lean_alloc_sarray(1, 0, size)`, whose capacity
/// libuv reads into).
pub enum RecvTarget<'a> {
    /// The array's `size` bytes, uninitialized: the crate reads into them,
    /// and the translator sets the array's size to the count it gets back.
    Uninit(&'a mut [MaybeUninit<u8>]),
    /// An empty `Vec` with room for `size` bytes (leanrs's `ByteArray`): the
    /// crate reads into its spare capacity, at most `size` bytes, and sets
    /// its length.
    Vec(&'a mut Vec<u8>, usize),
}

impl RecvTarget<'_> {
    /// The buffer's length libuv's allocation callback reports (0: libuv
    /// calls back with `UV_ENOBUFS` instead of reading).
    pub(crate) fn len(&self) -> usize {
        match self {
            RecvTarget::Uninit(b) => b.len(),
            RecvTarget::Vec(_, n) => *n,
        }
    }
}

/// The translator's new `ByteArray` for one receive (see [`RecvTarget`]; in
/// threads mode the loop thread reads into it and hands it to `done`:
/// [`MaybeSend`] is `Send` there). `target` runs while the crate holds the
/// socket's state: it must only hand out the storage, not call into the
/// crate or yield. (The `alloc` closure of `recv`, which makes the buffer,
/// runs with no socket held and may end the process through the glue's
/// internal panic; in threads mode with the loop lock held, as natively.)
pub trait RecvBuf: MaybeSend + 'static {
    /// Its storage.
    fn target(&mut self) -> RecvTarget<'_>;
}

/// A `Vec` made with `Vec::with_capacity(size)` (the driver's and leanrs's
/// `ByteArray`): its spare capacity is the size asked for.
impl RecvBuf for Vec<u8> {
    fn target(&mut self) -> RecvTarget<'_> {
        let n = self.capacity() - self.len();
        RecvTarget::Vec(self, n)
    }
}

/// One `read(2)` (`from` false) or `recvfrom(2)` (`from` true) of `fd` into
/// `t`, retried on `EINTR`: the count, and the sender for `recvfrom`. A
/// [`RecvTarget::Vec`] gets the bytes appended (its length set to them).
pub(crate) fn recv_into(
    fd: BorrowedFd<'_>,
    t: RecvTarget<'_>,
    from: bool,
) -> rustix::io::Result<(usize, Option<SocketAddrAny>)> {
    use rustix::io::Errno;
    use rustix::net::{recvfrom, RecvFlags};
    let none = RecvFlags::empty();
    match t {
        RecvTarget::Uninit(b) => loop {
            let r = if from {
                recvfrom(fd, &mut *b, none).map(|((i, _), _, a)| (i.len(), a))
            } else {
                rustix::io::read(fd, &mut *b).map(|(i, _)| (i.len(), None))
            };
            if r != Err(Errno::INTR) {
                return r;
            }
        },
        RecvTarget::Vec(v, n) if v.capacity() - v.len() == n => loop {
            let r = if from {
                recvfrom(fd, rustix::buffer::spare_capacity(v), none).map(|(k, _, a)| (k, a))
            } else {
                rustix::io::read(fd, rustix::buffer::spare_capacity(v)).map(|k| (k, None))
            };
            if r != Err(Errno::INTR) {
                return r;
            }
        },
        RecvTarget::Vec(v, n) => {
            // more room than asked for: read into `n` zeroed bytes, so that
            // no more than `n` come
            let at = v.len();
            v.resize(at + n, 0);
            let r = loop {
                let b = &mut v[at..];
                let r = if from {
                    recvfrom(fd, b, none).map(|(k, _, a)| (k, a))
                } else {
                    rustix::io::read(fd, b).map(|k| (k, None))
                };
                if r != Err(Errno::INTR) {
                    break r;
                }
            };
            v.truncate(at + r.as_ref().map_or(0, |(k, _)| (*k).min(n)));
            r
        }
    }
}

/// A socket's descriptor (shared with the loop's watch while one is
/// registered).
pub(crate) type Fd = Shared<OwnedFd>;

/// Close a socket's descriptor: a standard descriptor (0 to 2) is left
/// open, as libuv's `uv__stream_close` leaves it. libuv's `uv__udp_close`
/// calls `uv__close`, whose `assert(fd > STDERR_FILENO)` aborts the process
/// for a UDP socket on 0 to 2; here that socket is left open too (a known
/// difference, `docs/net.md`; out of reach in practice, since native's
/// startup descriptors take 0 to 2 first when they are closed at start).
pub(crate) fn close_fd(fd: Option<Fd>) {
    if let Some(fd) = fd {
        if rustix::fd::AsRawFd::as_raw_fd(&*fd) <= 2 {
            if let Ok(owned) = Shared::try_unwrap(fd) {
                std::mem::forget(owned);
            }
        }
    }
}
