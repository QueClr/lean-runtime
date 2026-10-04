//! `Std.Internal.UV.UDP.Socket` (Lean 4.34.0 `src/runtime/uv/udp.cpp`) over
//! libuv 1.48's `uv_udp_t` (`src/unix/udp.c`), on the scheduler's event
//! loop.
//!
//! What a Lean program sees, in short (cases `tests/cases/net/udp_*`):
//! - a new socket has no descriptor; `bind` makes one (with `SO_REUSEADDR`),
//!   and so do `connect`, `send` with an address, `recv`, `waitReadable` and
//!   `setMembership`, which bind it first to the wildcard address of the
//!   family (port 0); the option setters and `getSockName` on a socket
//!   without one fail with `EBADF`;
//! - a datagram is the concatenation of a `send`'s buffers, sent at once if
//!   the socket takes it; the promise resolves on the loop;
//! - `send` with an address on a connected socket fails with `EISCONN`,
//!   without one on an unconnected socket with `EDESTADDRREQ`;
//! - `recv n` gives at most `n` bytes of the next datagram (the rest of it
//!   is lost) and its sender; one receive at a time (`EALREADY`).

use super::{
    close_fd, feed, on_socket, sockaddr, socket_address, uv_err, uv_error, with_code, Done, Ev, Fd,
    Handle, IoWatcher, RecvBuf, RecvTarget, SendData, SocketId, UV_EAFNOSUPPORT, UV_EAGAIN,
    UV_EALREADY, UV_EBADF, UV_EDESTADDRREQ, UV_EINVAL, UV_EISCONN, UV_ENOBUFS,
};
use crate::io::IoError;
use crate::sched::{Interest, Ready};
use rustix::fd::{AsFd, OwnedFd};
use rustix::io::Errno;
use rustix::net::{
    sockopt, AddressFamily, MMsgHdr, SendAncillaryBuffer, SendFlags, SocketAddrAny, SocketFlags,
    SocketType,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::IoSlice;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::rc::Rc;

/// `Std.Internal.UV.UDP.Socket`: a translator keeps one in its external
/// object (clones name the same socket). The socket closes when the last
/// clone goes and no operation is pending (Lean's finalizer; `Handle`).
#[derive(Clone)]
pub struct UdpSocket(Rc<Handle<Udp>>);

/// The outcome of one libuv receive callback.
enum RecvOutcome {
    /// `nread >= 0` bytes, from the sender (none: nothing was there after
    /// all, libuv's `EAGAIN` call).
    Data(usize, Option<SocketAddr>),
    /// `UV_ENOBUFS`: the buffer had no room (`waitReadable`'s answer).
    NoBufs,
    Error(i32),
}

/// A receive in progress (Lean's `m_promise_read`).
enum ReadReq {
    Recv(Box<dyn RecvOp>),
    Wait(Done<()>),
}

trait RecvOp {
    fn target(&mut self) -> RecvTarget<'_>;
    fn finish(self: Box<Self>, r: RecvOutcome);
}

/// What `recv` resolves with: the translator's buffer and the count read,
/// and the sender.
pub type Datagram<B> = (B, usize, Option<SocketAddr>);

type RecvDone<B> = Box<dyn FnOnce(Result<Datagram<B>, IoError>)>;

struct Recv<B: RecvBuf> {
    buf: B,
    done: RecvDone<B>,
}

impl<B: RecvBuf> RecvOp for Recv<B> {
    fn target(&mut self) -> RecvTarget<'_> {
        self.buf.target()
    }
    /// `lean_uv_udp_recv`'s callback.
    fn finish(self: Box<Self>, r: RecvOutcome) {
        let Recv { buf, done } = *self;
        done(match r {
            RecvOutcome::Data(n, a) => Ok((buf, n, a)),
            RecvOutcome::NoBufs => Err(uv_error(UV_ENOBUFS)),
            RecvOutcome::Error(e) => Err(uv_error(e)),
        })
    }
}

fn finish_read(r: ReadReq, o: RecvOutcome) {
    match r {
        ReadReq::Recv(op) => op.finish(o),
        // `lean_uv_udp_wait_readable`'s callback
        ReadReq::Wait(done) => done(match o {
            RecvOutcome::NoBufs => Ok(()),
            RecvOutcome::Error(e) => Err(uv_error(e)),
            // native asserts this never happens (no buffer, no read)
            RecvOutcome::Data(..) => Ok(()),
        }),
    }
}

/// A queued datagram (`uv_udp_send_t`).
struct SendReq {
    data: Box<dyn SendData>,
    /// The destination (none: the connected peer).
    addr: Option<SocketAddrAny>,
    /// Bytes sent, or libuv's code.
    status: i32,
    done: Done<()>,
    _keep: UdpSocket,
}

#[derive(Default)]
struct Udp {
    /// The socket's number in the registry, which the loop's callbacks hold.
    id: SocketId,
    fd: Option<Fd>,
    ipv6: bool,
    bound: bool,
    connected: bool,
    /// Lean's `m_promise_read`; libuv's receive callback is set (`POLLIN`
    /// watched) exactly while it is.
    read: Option<(ReadReq, UdpSocket)>,
    write_queue: VecDeque<SendReq>,
    write_completed: VecDeque<SendReq>,
    send_queue_count: usize,
    /// `UV_HANDLE_UDP_PROCESSING`: the send callbacks are running.
    processing: bool,
    watcher: IoWatcher,
}

impl Drop for Udp {
    /// Lean's finalizer: `uv_close` (`uv__udp_close`) closes the descriptor
    /// at once, when the handle goes (or a loop callback running on the
    /// socket returns after that). No operation is pending: each holds the
    /// handle.
    fn drop(&mut self) {
        self.watcher.close();
        close_fd(self.fd.take());
    }
}

/// `uv__setsockopt_maybe_char`'s range check: an `int` from 0 to 255.
fn char_range(v: u32) -> Result<(), IoError> {
    if (v as i32) < 0 || v > 255 {
        return Err(uv_error(UV_EINVAL));
    }
    Ok(())
}

impl Udp {
    fn fd(&self) -> Result<&OwnedFd, IoError> {
        self.fd.as_deref().ok_or_else(|| uv_error(UV_EBADF))
    }

    fn io_set(&mut self, want: Interest) {
        let id = self.id;
        self.watcher.set(self.fd.as_ref(), want, || {
            Rc::new(move |r: Ready| {
                on_socket(id, |s: &RefCell<Udp>| {
                    let ev = Loop::events(s, r);
                    Loop::on_io(s, ev);
                })
            })
        });
    }

    fn io_start(&mut self, read: bool, write: bool) {
        let mut w = self.watcher.want();
        w.read |= read;
        w.write |= write;
        self.io_set(w);
    }

    fn io_stop(&mut self, read: bool, write: bool) {
        let mut w = self.watcher.want();
        w.read &= !read;
        w.write &= !write;
        self.io_set(w);
    }

    fn feed(&mut self) {
        feed(&mut self.watcher, self.id, Loop::on_io, |u: &mut Udp| {
            &mut u.watcher
        });
    }

    /// `uv__udp_bind(handle, addr, flags)`: a descriptor of the address's
    /// family if there is none (kept even if the bind then fails),
    /// `SO_REUSEADDR` if asked, then `bind`.
    fn bind(&mut self, addr: &SocketAddr, reuse: bool) -> Result<(), i32> {
        if self.fd.is_none() {
            let family = if addr.is_ipv6() {
                AddressFamily::INET6
            } else {
                AddressFamily::INET
            };
            let fd = rustix::net::socket_with(
                family,
                SocketType::DGRAM,
                SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
                None,
            )
            .map_err(uv_err)?;
            self.fd = Some(Rc::new(fd));
        }
        let fd = self.fd.clone().expect("made above");
        if reuse {
            // `uv__set_reuse` on Linux
            sockopt::set_socket_reuseaddr(&*fd, true).map_err(uv_err)?;
        }
        match rustix::net::bind(&*fd, &sockaddr(addr)) {
            Ok(()) => {}
            Err(Errno::AFNOSUPPORT) => return Err(UV_EINVAL),
            Err(e) => return Err(uv_err(e)),
        }
        if addr.is_ipv6() {
            self.ipv6 = true;
        }
        self.bound = true;
        Ok(())
    }

    /// `uv__udp_maybe_deferred_bind`: a socket without a descriptor is bound
    /// to the wildcard address of `v6`'s family, port 0.
    fn deferred_bind(&mut self, v6: bool, reuse: bool) -> Result<(), i32> {
        if self.fd.is_some() {
            return Ok(());
        }
        let any = if v6 {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        };
        self.bind(&any, reuse)
    }

    /// `uv__udp_sendmsg` (Linux: `sendmmsg` of up to 20 queued datagrams at a
    /// time). A batch that fails before its first datagram fails whole, with
    /// that error, unless the socket is full (`EAGAIN`, `ENOBUFS`: the
    /// datagrams wait for `POLLOUT`).
    fn sendmsg(&mut self) {
        if self.write_queue.is_empty() {
            return;
        }
        let fd = self.fd.clone().expect("a sending socket has a descriptor");
        loop {
            let pkts = self.write_queue.len().min(20);
            let r = {
                let iovs: Vec<Vec<IoSlice<'_>>> = self
                    .write_queue
                    .iter()
                    .take(pkts)
                    .map(|q| {
                        (0..q.data.count())
                            .map(|i| IoSlice::new(q.data.get(i)))
                            .collect()
                    })
                    .collect();
                let mut controls: Vec<SendAncillaryBuffer<'_, '_, '_>> =
                    (0..pkts).map(|_| SendAncillaryBuffer::default()).collect();
                let mut msgs: Vec<MMsgHdr<'_>> = self
                    .write_queue
                    .iter()
                    .zip(iovs.iter())
                    .zip(controls.iter_mut())
                    .map(|((q, iov), c)| match &q.addr {
                        Some(a) => MMsgHdr::new_with_addr(a, iov, c),
                        None => MMsgHdr::new(iov, c),
                    })
                    .collect();
                loop {
                    match rustix::net::sendmmsg(&*fd, &mut msgs, SendFlags::empty()) {
                        Err(Errno::INTR) => continue,
                        r => break r,
                    }
                }
            };
            match r {
                Ok(n) if n >= 1 => {
                    for _ in 0..n.min(self.write_queue.len()) {
                        let mut q = self.write_queue.pop_front().expect("sent");
                        q.status = 0;
                        self.write_completed.push_back(q);
                    }
                    if !self.write_queue.is_empty() {
                        continue;
                    }
                    self.feed();
                    return;
                }
                Err(Errno::AGAIN) | Err(Errno::NOBUFS) => return,
                r => {
                    let e = match r {
                        Err(e) => uv_err(e),
                        Ok(_) => UV_EAGAIN,
                    };
                    for _ in 0..pkts {
                        let mut q = self.write_queue.pop_front().expect("queued");
                        q.status = e;
                        self.write_completed.push_back(q);
                    }
                    self.feed();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
impl UdpSocket {
    /// A `uv__io_feed` now, with nothing pending: the state a write that
    /// finished on the loop leaves behind (the unit tests').
    pub(crate) fn feed_for_tests(&self) {
        self.0.borrow_mut().feed();
    }
}

impl UdpSocket {
    /// `Socket.new` (`lean_uv_udp_new`, `uv_udp_init`): a socket with no
    /// descriptor yet.
    pub fn new() -> Result<UdpSocket, IoError> {
        crate::io::effect_point();
        super::loop_lock();
        Ok(UdpSocket(Rc::new(Handle::new(|id| {
            let mut t = Udp::default();
            t.id = id;
            t
        }))))
    }
}

// ---------------------------------------------------------------------------
// The loop's side (`uv__udp_io`)

/// A socket's state as a loop callback sees it ([`super::on_socket`]).
struct Loop<'a>(&'a RefCell<Udp>);

impl Loop<'_> {
    /// The events libuv hands `uv__udp_io` ([`Ev::libuv_merge`]). The
    /// loop's `Ready` folds an error or a hang-up into both directions, so
    /// then the raw `POLLIN` and `POLLOUT` are asked again with `poll(2)`.
    fn events(s: &RefCell<Udp>, r: Ready) -> Ev {
        let u = s.borrow();
        let want = u.watcher.want();
        if !r.error && !r.hangup {
            return Ev::libuv_merge(r.read, r.write, false, false, want);
        }
        let Some(fd) = u.fd.clone() else {
            return Ev::default();
        };
        drop(u);
        use rustix::event::{poll, PollFd, PollFlags, Timespec};
        let mut p = [PollFd::new(&*fd, PollFlags::IN | PollFlags::OUT)];
        let zero = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if poll(&mut p, Some(&zero)).is_err() {
            return Ev::libuv_merge(false, false, r.error, r.hangup, want);
        }
        let f = p[0].revents();
        Ev::libuv_merge(
            f.contains(PollFlags::IN),
            f.contains(PollFlags::OUT),
            f.contains(PollFlags::ERR),
            f.contains(PollFlags::HUP),
            want,
        )
    }

    /// The watch callback and `uv__io_feed`'s call: `uv__udp_io`.
    fn on_io(s: &RefCell<Udp>, ev: Ev) {
        let u = Loop(s);
        if ev.read {
            u.recvmsg();
        }
        if ev.write {
            s.borrow_mut().sendmsg();
            u.run_completed();
        }
    }

    /// `uv__udp_recvmsg` with Lean's receive callbacks (which stop receiving
    /// first, `uv_udp_recv_stop`).
    fn recvmsg(&self) {
        let mut count = 32;
        loop {
            let mut u = self.0.borrow_mut();
            if u.read.is_none() || count == 0 {
                return;
            }
            let Some(fd) = u.fd.clone() else {
                return;
            };
            let (req, _) = u.read.as_mut().expect("checked");
            let outcome = match req {
                ReadReq::Wait(_) => RecvOutcome::NoBufs,
                ReadReq::Recv(op) => {
                    let target = op.target();
                    if target.len() == 0 {
                        RecvOutcome::NoBufs
                    } else {
                        match super::recv_into(fd.as_fd(), target, true) {
                            Ok((n, a)) => RecvOutcome::Data(n, a.and_then(socket_address)),
                            Err(Errno::AGAIN) => RecvOutcome::Data(0, None),
                            Err(e) => RecvOutcome::Error(uv_err(e)),
                        }
                    }
                }
            };
            count -= 1;
            // libuv goes on while the read gave a datagram (not `EAGAIN`, not
            // an error) and a callback is set again
            let more = matches!(outcome, RecvOutcome::Data(_, Some(_)));
            let nobufs = matches!(outcome, RecvOutcome::NoBufs);
            u.io_stop(true, false);
            let (req, keep) = u.read.take().expect("checked");
            drop(u);
            finish_read(req, outcome);
            drop(keep);
            if nobufs || !more {
                return;
            }
        }
    }

    /// `uv__udp_run_completed`: the sent datagrams' promises, in order; then
    /// the loop stops waiting for `POLLOUT` if nothing is queued.
    fn run_completed(&self) {
        {
            let mut u = self.0.borrow_mut();
            if u.processing {
                return;
            }
            u.processing = true;
        }
        loop {
            let q = {
                let mut u = self.0.borrow_mut();
                let q = u.write_completed.pop_front();
                if q.is_some() {
                    u.send_queue_count -= 1;
                }
                q
            };
            let Some(q) = q else {
                break;
            };
            let SendReq {
                data,
                status,
                done,
                _keep,
                ..
            } = q;
            drop(data);
            // Lean's send callback: resolve, then let go of the socket
            done(with_code(if status >= 0 { 0 } else { status }));
            drop(_keep);
        }
        let mut u = self.0.borrow_mut();
        if u.write_queue.is_empty() {
            u.io_stop(false, true);
        }
        u.processing = false;
    }
}

// ---------------------------------------------------------------------------
// The externs

impl UdpSocket {
    /// `Socket.bind` (`lean_uv_udp_bind`, `uv_udp_bind(..., UV_UDP_REUSEADDR)`).
    pub fn bind(&self, addr: SocketAddr) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        self.0.borrow_mut().bind(&addr, true).map_err(uv_error)
    }

    /// `Socket.connect` (`lean_uv_udp_connect`, `uv_udp_connect`).
    pub fn connect(&self, addr: SocketAddr) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        let mut u = self.0.borrow_mut();
        if u.connected {
            return Err(uv_error(UV_EISCONN));
        }
        u.deferred_bind(addr.is_ipv6(), false).map_err(uv_error)?;
        let fd = u.fd.clone().expect("bound above");
        loop {
            match rustix::net::connect(&*fd, &sockaddr(&addr)) {
                Err(Errno::INTR) => continue,
                Err(e) => return Err(uv_error(uv_err(e))),
                Ok(()) => break,
            }
        }
        u.connected = true;
        Ok(())
    }

    /// `Socket.send` (`lean_uv_udp_send`, `uv_udp_send`): one datagram of
    /// `data`'s buffers to `addr` (none: the connected peer), sent at once if
    /// the socket takes it; `done` gets the outcome on the loop. An empty
    /// array resolves at once.
    pub fn send<D: SendData>(
        &self,
        data: D,
        addr: Option<SocketAddr>,
        done: impl FnOnce(Result<(), IoError>) + 'static,
    ) -> Result<(), IoError> {
        crate::io::effect_point();
        if data.count() == 0 {
            done(Ok(()));
            return Ok(());
        }
        super::loop_lock();
        let mut u = self.0.borrow_mut();
        // `uv__udp_check_before_send`
        if addr.is_some() && u.connected {
            return Err(uv_error(UV_EISCONN));
        }
        if addr.is_none() && !u.connected {
            return Err(uv_error(UV_EDESTADDRREQ));
        }
        if let Some(a) = &addr {
            u.deferred_bind(a.is_ipv6(), false).map_err(uv_error)?;
        }
        let empty_queue = u.send_queue_count == 0;
        u.write_queue.push_back(SendReq {
            data: Box::new(data),
            addr: addr.as_ref().map(sockaddr),
            status: 0,
            done: Box::new(done),
            _keep: self.clone(),
        });
        u.send_queue_count += 1;
        if empty_queue && !u.processing {
            u.sendmsg();
            if !u.write_queue.is_empty() {
                u.io_start(false, true);
            }
        } else {
            u.io_start(false, true);
        }
        Ok(())
    }

    /// `Socket.recv` (`lean_uv_udp_recv`, `uv_udp_recv_start`): `alloc` makes
    /// the translator's `ByteArray` of the size asked for, after the check
    /// for a receive in progress; `done` gets the next datagram (at most that
    /// size; the rest of it is lost) and its sender, on the loop. A socket
    /// without a descriptor is bound first (IPv4 wildcard, port 0).
    pub fn recv<B: RecvBuf>(
        &self,
        alloc: impl FnOnce() -> B,
        done: impl FnOnce(Result<Datagram<B>, IoError>) + 'static,
    ) -> Result<(), IoError> {
        super::loop_lock();
        if self.0.borrow().read.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        // Lean's allocation of the buffer, at the call (`lean_alloc_sarray`),
        // with the socket not borrowed (review RNET-01, as in TCP's `recv`).
        // A size of 0 fails with `ENOBUFS` once a datagram is there,
        // leaving it (as natively; case `udp_recv_zero`).
        let buf = alloc();
        let mut u = self.0.borrow_mut();
        if u.read.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        Self::recv_start(&mut u)?;
        let op: Box<dyn RecvOp> = Box::new(Recv {
            buf,
            done: Box::new(done),
        });
        u.read = Some((ReadReq::Recv(op), self.clone()));
        Ok(())
    }

    /// `Socket.waitReadable` (`lean_uv_udp_wait_readable`): `done` gets `()`
    /// once a datagram is there (not read).
    pub fn wait_readable(
        &self,
        done: impl FnOnce(Result<(), IoError>) + 'static,
    ) -> Result<(), IoError> {
        super::loop_lock();
        let mut u = self.0.borrow_mut();
        if u.read.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        Self::recv_start(&mut u)?;
        u.read = Some((ReadReq::Wait(Box::new(done)), self.clone()));
        Ok(())
    }

    /// `uv__udp_recv_start`.
    fn recv_start(u: &mut Udp) -> Result<(), IoError> {
        if u.watcher.want().read {
            return Err(uv_error(UV_EALREADY));
        }
        u.deferred_bind(false, false).map_err(uv_error)?;
        u.io_start(true, false);
        Ok(())
    }

    /// `Socket.cancelRecv` (`lean_uv_udp_cancel_recv`): stop a pending
    /// receive and let go of its promise without resolving it, and of the
    /// socket (LEAN-BUG LB-24: native keeps the socket's reference, so the
    /// socket and its descriptor are never freed).
    pub fn cancel_recv(&self) {
        super::loop_lock();
        let r = {
            let mut u = self.0.borrow_mut();
            if u.read.is_none() {
                return;
            }
            u.io_stop(true, false);
            u.read.take()
        };
        drop(r);
    }

    fn name(&self, peer: bool) -> Result<SocketAddr, IoError> {
        let u = self.0.borrow();
        let fd = u.fd()?;
        let a = if peer {
            rustix::net::getpeername(fd).map_err(|e| uv_error(uv_err(e)))?
        } else {
            Some(rustix::net::getsockname(fd).map_err(|e| uv_error(uv_err(e)))?)
        };
        a.and_then(socket_address)
            .ok_or_else(|| uv_error(UV_EAFNOSUPPORT))
    }

    /// `Socket.getPeerName` (`lean_uv_udp_getpeername`).
    pub fn peer_name(&self) -> Result<SocketAddr, IoError> {
        super::loop_lock();
        self.name(true)
    }

    /// `Socket.getSockName` (`lean_uv_udp_getsockname`).
    pub fn sock_name(&self) -> Result<SocketAddr, IoError> {
        super::loop_lock();
        self.name(false)
    }

    /// `Socket.setBroadcast` (`uv_udp_set_broadcast`).
    pub fn set_broadcast(&self, on: bool) -> Result<(), IoError> {
        super::loop_lock();
        let u = self.0.borrow();
        sockopt::set_socket_broadcast(u.fd()?, on).map_err(|e| uv_error(uv_err(e)))
    }

    /// `Socket.setTTL` (`uv_udp_set_ttl`): 1 to 255 (Lean's `UInt32` as a C
    /// `int`), `IP_TTL`, or `IPV6_UNICAST_HOPS` on a socket bound to IPv6.
    pub fn set_ttl(&self, ttl: u32) -> Result<(), IoError> {
        super::loop_lock();
        if (ttl as i32) < 1 || ttl > 255 {
            return Err(uv_error(UV_EINVAL));
        }
        let u = self.0.borrow();
        let fd = u.fd()?;
        let r = if u.ipv6 {
            sockopt::set_ipv6_unicast_hops(fd, Some(ttl as u8))
        } else {
            sockopt::set_ip_ttl(fd, ttl)
        };
        r.map_err(|e| uv_error(uv_err(e)))
    }

    /// `Socket.setMulticastTTL` (`uv_udp_set_multicast_ttl`): 0 to 255.
    pub fn set_multicast_ttl(&self, ttl: u32) -> Result<(), IoError> {
        super::loop_lock();
        char_range(ttl)?;
        let u = self.0.borrow();
        let fd = u.fd()?;
        let r = if u.ipv6 {
            sockopt::set_ipv6_multicast_hops(fd, ttl)
        } else {
            sockopt::set_ip_multicast_ttl(fd, ttl)
        };
        r.map_err(|e| uv_error(uv_err(e)))
    }

    /// `Socket.setMulticastLoop` (`uv_udp_set_multicast_loop`).
    pub fn set_multicast_loop(&self, on: bool) -> Result<(), IoError> {
        super::loop_lock();
        let u = self.0.borrow();
        let fd = u.fd()?;
        let r = if u.ipv6 {
            sockopt::set_ipv6_multicast_loop(fd, on)
        } else {
            sockopt::set_ip_multicast_loop(fd, on)
        };
        r.map_err(|e| uv_error(uv_err(e)))
    }

    /// `Socket.setMembership` (`lean_uv_udp_set_membership`,
    /// `uv_udp_set_membership`): join (`membership` 1) or leave (0) the
    /// group; any other value fails with `EINVAL` after the socket is bound
    /// (wildcard of the group's family, port 0, `SO_REUSEADDR`) if it had no
    /// descriptor. An IPv4 group's interface is an IPv4 address (an IPv6 one
    /// fails with `EINVAL`); an IPv6 group's interface is an IPv6 address
    /// whose only use is its scope, which a Lean address never has (the
    /// default interface), and an IPv4 one fails with `EINVAL`.
    pub fn set_membership(
        &self,
        group: IpAddr,
        interface: Option<IpAddr>,
        membership: u8,
    ) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        // LEAN-BUG LB-22: native writes each address's text into a 16-byte
        // buffer (`INET_ADDRSTRLEN`, also for IPv6) and aborts
        // (`lean_always_assert`) when an IPv6 text is 16 bytes or longer;
        // here every address works.
        let mut u = self.0.borrow_mut();
        match group {
            IpAddr::V4(g) => {
                u.deferred_bind(false, true).map_err(uv_error)?;
                let ifa = match interface {
                    None => Ipv4Addr::UNSPECIFIED,
                    Some(IpAddr::V4(i)) => i,
                    Some(IpAddr::V6(_)) => return Err(uv_error(UV_EINVAL)),
                };
                let fd = u.fd()?;
                let r = match membership {
                    1 => sockopt::set_ip_add_membership(fd, &g, &ifa),
                    0 => sockopt::set_ip_drop_membership(fd, &g, &ifa),
                    _ => return Err(uv_error(UV_EINVAL)),
                };
                r.map_err(|e| uv_error(uv_err(e)))
            }
            IpAddr::V6(g) => {
                u.deferred_bind(true, true).map_err(uv_error)?;
                if let Some(IpAddr::V4(_)) = interface {
                    return Err(uv_error(UV_EINVAL));
                }
                let fd = u.fd()?;
                let r = match membership {
                    1 => sockopt::set_ipv6_add_membership(fd, &g, 0),
                    0 => sockopt::set_ipv6_drop_membership(fd, &g, 0),
                    _ => return Err(uv_error(UV_EINVAL)),
                };
                r.map_err(|e| uv_error(uv_err(e)))
            }
        }
    }

    /// `Socket.setMulticastInterface` (`lean_uv_udp_set_multicast_interface`,
    /// `uv_udp_set_multicast_interface`): `IP_MULTICAST_IF` for an IPv4
    /// address, `IPV6_MULTICAST_IF` with the address's scope (0) for an IPv6
    /// one.
    pub fn set_multicast_interface(&self, interface: IpAddr) -> Result<(), IoError> {
        super::loop_lock();
        // LEAN-BUG LB-22: as `set_membership`, no abort on a long IPv6
        // text.
        let u = self.0.borrow();
        let fd = u.fd()?;
        let r = match interface {
            IpAddr::V4(a) => sockopt::set_ip_multicast_if(fd, &a),
            IpAddr::V6(_) => sockopt::set_ipv6_multicast_if(fd, 0),
        };
        r.map_err(|e| uv_error(uv_err(e)))
    }
}
