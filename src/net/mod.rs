//! Networking (feature `net`): Lean 4.34.0's TCP, UDP, DNS and interface
//! externs (`src/runtime/uv/tcp.cpp`, `udp.cpp`, `dns.cpp`, `net_addr.cpp`,
//! over libuv 1.48), on the scheduler's event loop.
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
//! **Lifetime.** A socket closes when the translator's last handle goes
//! (Lean's finalizer, `uv_close`), unless an operation is pending: a pending
//! operation holds the socket, as native's `lean_inc(socket)`.
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
use crate::sched::{self, Interest, Ready, WatchId};
use rustix::fd::{BorrowedFd, OwnedFd};
use rustix::net::SocketAddrAny;
use std::cell::RefCell;
use std::mem::MaybeUninit;

use std::rc::{Rc, Weak};
use std::time::Instant;

pub mod dns;
pub mod iface;
pub mod tcp;
pub mod udp;

#[cfg(test)]
mod tests;

pub use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

/// A pending operation's completion: it holds the translator's promise
/// (see the module comment).
pub type Done<T> = Box<dyn FnOnce(Result<T, IoError>)>;

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

/// `event_loop_lock`: natively an extern takes the loop's lock, which makes
/// the loop thread finish its iteration first, so the callbacks of what
/// became ready meanwhile run before the extern acts. Here the loop context
/// runs what is due (`sched::catch_up`, as `sched::uv`'s externs do). Called
/// with no socket borrowed: the callbacks may use the socket.
#[inline]
pub(crate) fn loop_lock() {
    sched::catch_up();
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
/// (`uv__io_start`, `uv__io_stop`), kept in step with one `sched::watch` of
/// the descriptor (none while it waits for nothing).
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
        fd: Option<&Rc<OwnedFd>>,
        want: Interest,
        cb: impl FnOnce() -> Rc<dyn Fn(Ready)>,
    ) {
        let Some(fd) = fd else {
            self.want = want;
            return;
        };
        let empty = !want.read && !want.write;
        match self.id {
            Some(id) if empty => {
                sched::unwatch(id);
                self.id = None;
            }
            Some(id) if want != self.want => {
                // the watch is this handle's: `NOENT` cannot happen
                let _ = sched::watch_modify(id, want);
            }
            None if !empty => {
                // epoll refusing (no memory) is libuv's abort; here the
                // handle just waits on
                self.id = sched::watch(fd.clone(), want, cb()).ok();
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
            sched::unwatch(id);
        }
        self.want = Interest::default();
    }
}

/// `uv__io_feed`: run `io(FEED)` on the loop context soon (once until it has
/// run), as libuv's pending queue runs the watcher with `POLLOUT` at its
/// next turn.
pub(crate) fn feed<T: 'static>(
    w: &mut IoWatcher,
    me: &Weak<RefCell<T>>,
    io: fn(&Rc<RefCell<T>>, Ev),
    fed: fn(&mut T) -> &mut IoWatcher,
) {
    if w.fed {
        return;
    }
    w.fed = true;
    let me = me.clone();
    sched::timer_start(
        Instant::now(),
        Rc::new(move || {
            if let Some(s) = me.upgrade() {
                fed(&mut s.borrow_mut()).fed = false;
                io(&s, Ev::FEED);
            }
        }),
    );
}

/// The watch callback of a handle: `io(events)` while the handle lives.
pub(crate) fn watch_cb<T: 'static>(
    me: &Weak<RefCell<T>>,
    io: fn(&Rc<RefCell<T>>, Ev),
    want: fn(&T) -> Interest,
    conv: fn(Ready, Interest) -> Ev,
) -> Rc<dyn Fn(Ready)> {
    let me = me.clone();
    Rc::new(move |r| {
        if let Some(s) = me.upgrade() {
            let w = want(&s.borrow());
            io(&s, conv(r, w));
        }
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
/// the crate until the write completes. Its methods run while the crate
/// holds the socket's state: they must only read the bytes, not call into
/// the crate or yield (no effect point, no wait).
pub trait SendData: 'static {
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

/// The translator's new `ByteArray` for one receive (see [`RecvTarget`]).
/// `target` runs while the crate holds the socket's state: it must only
/// hand out the storage, not call into the crate or yield. (The `alloc`
/// closure of `recv`, which makes the buffer, runs with nothing held and
/// may end the process through the glue's internal panic.)
pub trait RecvBuf: 'static {
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
pub(crate) type Fd = Rc<OwnedFd>;

/// Close a socket's descriptor: a standard descriptor (0 to 2) is left
/// open, as libuv's `uv__stream_close` leaves it. libuv's `uv__udp_close`
/// calls `uv__close`, whose `assert(fd > STDERR_FILENO)` aborts the process
/// for a UDP socket on 0 to 2; here that socket is left open too (a known
/// difference, `docs/net.md`; out of reach in practice, since native's
/// startup descriptors take 0 to 2 first when they are closed at start).
pub(crate) fn close_fd(fd: Option<Fd>) {
    if let Some(fd) = fd {
        if rustix::fd::AsRawFd::as_raw_fd(&*fd) <= 2 {
            if let Ok(owned) = Rc::try_unwrap(fd) {
                std::mem::forget(owned);
            }
        }
    }
}
