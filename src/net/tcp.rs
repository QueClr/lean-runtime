//! `Std.Internal.UV.TCP.Socket` (Lean 4.34.0 `src/runtime/uv/tcp.cpp`) over
//! libuv 1.48's stream model (`src/unix/tcp.c`, `stream.c`), on the
//! scheduler's event loop.
//!
//! A [`TcpSocket`] is Lean's `lean_uv_tcp_socket_object` and its `uv_tcp_t`
//! in one: libuv's flags (readable, writable, bound, shut, ...), its delayed
//! error, its connect, write and shutdown requests, the accepted descriptor
//! of a listening socket, and Lean's pending promises (`m_promise_read`,
//! `m_promise_accept`, `m_promise_shutdown`). The system calls are libuv's,
//! in its order; libuv's loop-thread work (`uv__stream_io`,
//! `uv__server_io`, the write callbacks) runs on the scheduler's loop
//! context.
//!
//! What a Lean program sees, in short (cases `tests/cases/net/tcp_*`):
//! - a new socket has no descriptor until `bind`, `connect` or `listen`
//!   makes one (non-blocking, close-on-exec): `getPeerName`, `getSockName`
//!   and `send` on it fail with `EBADF`, `recv?` and `shutdown` with
//!   `ENOTCONN`;
//! - `bind` sets `SO_REUSEADDR` (and clears `IPV6_V6ONLY` on IPv6); an
//!   address in use is not reported by `bind` but by the next `listen`,
//!   `connect`, `getSockName` or `getPeerName` (libuv's delayed error);
//! - one receive (`recv?` or `waitReadable`) at a time: a second fails with
//!   `EALREADY`; `cancelRecv` drops the pending one without resolving it;
//! - a `send` writes at once what the socket takes and queues the rest, in
//!   order; its promise resolves on the loop once all of it is written;
//! - `shutdown` stops further sends at once (`EPIPE`), and shuts the write
//!   side once the queued writes are done; the peer then reads end of file
//!   (`recv?` gives `none`, `waitReadable` `true`).

use super::{
    close_fd, feed, sockaddr, socket_address, uv_err, uv_error, watch_cb, with_code, Done, Ev, Fd,
    Handle, IoWatcher, RecvBuf, RecvTarget, SendData, SocketId, IOV_MAX, UV_EADDRINUSE,
    UV_EAFNOSUPPORT, UV_EAGAIN, UV_EALREADY, UV_EBADF, UV_ECANCELED, UV_ECONNREFUSED,
    UV_EINPROGRESS, UV_EINVAL, UV_ENOBUFS, UV_ENOTCONN, UV_EPIPE,
};
use crate::io::IoError;
use crate::sched::Interest;
use rustix::fd::{AsFd, OwnedFd};
use rustix::io::Errno;
use rustix::net::{sockopt, AddressFamily, SocketFlags, SocketType};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::IoSlice;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

/// `Std.Internal.UV.TCP.Socket`: a translator keeps one in its external
/// object (clones name the same socket). The socket closes when the last
/// clone goes and no operation is pending (Lean's finalizer; `Handle`).
#[derive(Clone)]
pub struct TcpSocket(Rc<Handle<Tcp>>);

/// A receive in progress: Lean's `m_promise_read` (and `m_byte_array`), with
/// libuv's read callback.
enum ReadReq {
    /// `recv?`: the translator's new `ByteArray`, then the promise.
    Recv(Box<dyn RecvOp>),
    /// `waitReadable`.
    Wait(Done<bool>),
}

/// `uv__write_req_update`'s walk over a request's buffers (`len(i)` bytes
/// each, `count` of them) from buffer `index`, `offset` bytes in, after `n`
/// more bytes were written: the new position. As libuv's `do ... while (n >
/// 0)`, an empty buffer at the position is passed over even when nothing
/// was written, but only one: so `k` empty buffers take `k` writes of 0
/// bytes (each after a `POLLOUT`).
pub(crate) fn advance(
    len: &dyn Fn(usize) -> usize,
    count: usize,
    mut index: usize,
    mut offset: usize,
    mut n: usize,
) -> (usize, usize) {
    while index < count {
        let l = len(index);
        let take = n.min(l - offset);
        offset += take;
        if offset == l {
            index += 1;
            offset = 0;
        }
        n -= take;
        if n == 0 {
            break;
        }
    }
    (index, offset)
}

/// The outcome of one libuv read callback.
enum ReadOutcome {
    /// `nread >= 0` bytes were read.
    Data(usize),
    Eof,
    /// `UV_ENOBUFS`: the buffer had no room (`waitReadable`'s answer).
    NoBufs,
    Error(i32),
}

/// A `recv?` of some translator's buffer type, boxed.
trait RecvOp {
    fn target(&mut self) -> RecvTarget<'_>;
    fn finish(self: Box<Self>, r: ReadOutcome);
}

type RecvDone<B> = Box<dyn FnOnce(Result<Option<(B, usize)>, IoError>)>;

struct Recv<B: RecvBuf> {
    buf: B,
    done: RecvDone<B>,
}

impl<B: RecvBuf> RecvOp for Recv<B> {
    fn target(&mut self) -> RecvTarget<'_> {
        self.buf.target()
    }
    /// `lean_uv_tcp_recv`'s read callback: the bytes read as `some`, end of
    /// file as `none`, an error as the error (`UV_ENOBUFS` included).
    fn finish(self: Box<Self>, r: ReadOutcome) {
        let Recv { buf, done } = *self;
        done(match r {
            ReadOutcome::Data(n) => Ok(Some((buf, n))),
            ReadOutcome::Eof => Ok(None),
            ReadOutcome::NoBufs => Err(uv_error(UV_ENOBUFS)),
            ReadOutcome::Error(e) => Err(uv_error(e)),
        })
    }
}

/// A queued write (`uv_write_t`): the buffers, how far they are written,
/// and the promise.
struct WriteReq {
    data: Box<dyn SendData>,
    /// `write_index`, and how much of that buffer is written.
    index: usize,
    offset: usize,
    error: i32,
    /// libuv freed `bufs` (a request finished without error): its bytes no
    /// longer count in `write_queue_size`.
    freed: bool,
    done: Done<()>,
    _keep: TcpSocket,
}

impl WriteReq {
    /// `uv__write_req_size`: the bytes not written yet.
    fn remaining(&self) -> usize {
        let n = self.data.count();
        let mut s = 0;
        for i in self.index..n {
            s += self.data.get(i).len();
        }
        s - self.offset.min(s)
    }
}

/// A pending operation's promise, and the socket it holds (native's
/// `lean_inc(socket)`).
struct Pending<T> {
    done: Done<T>,
    _keep: TcpSocket,
}

#[derive(Default)]
struct Tcp {
    /// The socket's number in the registry, which the loop's callbacks hold.
    id: SocketId,
    fd: Option<Fd>,
    // libuv's handle flags
    readable: bool,
    writable: bool,
    bound: bool,
    ipv6: bool,
    shut: bool,
    read_eof: bool,
    read_partial: bool,
    nodelay: bool,
    keepalive: bool,
    /// `uv_listen` succeeded: the watcher's callback is `uv__server_io`.
    listening: bool,
    delayed_error: i32,
    /// `connect_req`.
    connect: Option<Pending<()>>,
    write_queue: VecDeque<WriteReq>,
    write_queue_size: usize,
    write_completed: VecDeque<WriteReq>,
    /// `shutdown_req` (Lean's `m_promise_shutdown`).
    shutdown: Option<Pending<()>>,
    /// Lean's `m_promise_read`; libuv's `UV_HANDLE_READING` is
    /// `read.is_some()`.
    read: Option<(ReadReq, TcpSocket)>,
    /// Lean's `m_promise_accept`.
    accept: Option<Pending<TcpSocket>>,
    /// A connection the loop accepted that no `accept` took yet.
    accepted_fd: Option<OwnedFd>,
    watcher: IoWatcher,
}

impl Drop for Tcp {
    /// Lean's finalizer: `uv_close`, which closes the descriptors at once
    /// (`uv__stream_close`), when the handle goes (or a loop callback running
    /// on the socket returns after that). No operation is pending: each holds
    /// the handle.
    fn drop(&mut self) {
        self.watcher.close();
        close_fd(self.fd.take());
        self.accepted_fd = None;
    }
}

const UV_ENFILE: i32 = -23;
const UV_EMFILE: i32 = -24;
const UV_EBUSY: i32 = -16;

/// `uv__socket(domain, SOCK_STREAM, 0)`.
fn new_fd(family: AddressFamily) -> Result<OwnedFd, i32> {
    rustix::net::socket_with(
        family,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(uv_err)
}

fn family_of(a: &SocketAddr) -> AddressFamily {
    match a {
        SocketAddr::V4(_) => AddressFamily::INET,
        SocketAddr::V6(_) => AddressFamily::INET6,
    }
}

/// `uv__tcp_keepalive(fd, on, delay)` on Linux: `SO_KEEPALIVE`, then, when
/// on, the idle time, a 1 s interval and 10 probes. A delay of 0 with `on`
/// fails with `UV_EINVAL` after `SO_KEEPALIVE` is set.
fn tcp_keepalive(fd: &OwnedFd, on: bool, delay: u32) -> Result<(), i32> {
    sockopt::set_socket_keepalive(fd, on).map_err(uv_err)?;
    if !on {
        return Ok(());
    }
    if delay == 0 {
        // LEAN-BUG LB-23: libuv 1.48 returns -1 here, which is `UV_EPERM`
        // (`operation not permitted`); Lean's docstring and libuv 1.49 say
        // `UV_EINVAL`.
        return Err(UV_EINVAL);
    }
    sockopt::set_tcp_keepidle(fd, Duration::from_secs(delay.into())).map_err(uv_err)?;
    sockopt::set_tcp_keepintvl(fd, Duration::from_secs(1)).map_err(uv_err)?;
    sockopt::set_tcp_keepcnt(fd, 10).map_err(uv_err)?;
    Ok(())
}

/// libuv's spare descriptor for `accept`'s `EMFILE` (`loop->emfile_fd`),
/// kept for the life of the process.
static EMFILE_FD: Mutex<Option<OwnedFd>> = Mutex::new(None);

/// `uv__stream_init`'s reservation: while the loop has no spare descriptor,
/// a new stream opens `/dev/null` (or `/` if that fails) read-only and
/// close-on-exec. So the first TCP socket takes one descriptor more, as
/// natively.
fn reserve_emfile_fd() {
    let mut g = EMFILE_FD.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = open_cloexec_ro("/dev/null").or_else(|| open_cloexec_ro("/"));
    }
}

fn open_cloexec_ro(p: &str) -> Option<OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(p, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).ok()
}

/// `uv__accept`: `accept4(SOCK_NONBLOCK | SOCK_CLOEXEC)`, retried on
/// `EINTR`.
fn accept_fd(fd: &OwnedFd) -> Result<OwnedFd, i32> {
    loop {
        match rustix::net::accept_with(fd, SocketFlags::NONBLOCK | SocketFlags::CLOEXEC) {
            Err(Errno::INTR) => continue,
            r => return r.map_err(uv_err),
        }
    }
}

/// `uv__emfile_trick`: give up the spare descriptor, accept and close every
/// pending connection, take a spare again (on `/`).
fn emfile_trick(fd: &OwnedFd) -> i32 {
    let mut g = EMFILE_FD.lock().unwrap_or_else(|e| e.into_inner());
    if g.take().is_none() {
        return UV_EMFILE;
    }
    // `accept_fd` retries `EINTR` itself
    let err = loop {
        match accept_fd(fd) {
            Ok(c) => drop(c),
            Err(e) => break e,
        }
    };
    *g = open_cloexec_ro("/");
    err
}

/// libuv's `uv__ipv6_link_local_scope_id`: the scope of the first link-local
/// (`fe80::`) address `getifaddrs` lists, or 0.
fn link_local_scope_id() -> u32 {
    let Ok(addrs) = nix::ifaddrs::getifaddrs() else {
        return 0;
    };
    for a in addrs {
        if let Some(s6) = a.address.as_ref().and_then(|s| s.as_sockaddr_in6()) {
            let o = s6.ip().octets();
            if o[0] == 0xfe && o[1] == 0x80 {
                return s6.scope_id();
            }
        }
    }
    0
}

impl Tcp {
    fn fd(&self) -> Option<&OwnedFd> {
        self.fd.as_deref()
    }

    /// `uv__io_start` / `uv__io_stop`.
    fn io_set(&mut self, want: Interest) {
        let id = self.id;
        self.watcher.set(self.fd.as_ref(), want, || {
            watch_cb(id, Loop::on_io, |t: &Tcp| t.watcher.want(), Ev::from_ready)
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
        feed(&mut self.watcher, self.id, Loop::on_io, |t: &mut Tcp| {
            &mut t.watcher
        });
    }

    /// `uv__stream_open(fd, flags)` (after `uv__stream_init`): the handle's
    /// `TCP_NODELAY` and keep-alive (60 s) applied to the new descriptor.
    fn stream_open(&mut self, fd: OwnedFd, readable: bool, writable: bool) -> Result<(), i32> {
        if self.fd.is_some() {
            return Err(UV_EBUSY);
        }
        self.readable |= readable;
        self.writable |= writable;
        if self.nodelay {
            sockopt::set_tcp_nodelay(&fd, true).map_err(uv_err)?;
        }
        if self.keepalive {
            tcp_keepalive(&fd, true, 60)?;
        }
        self.fd = Some(Rc::new(fd));
        Ok(())
    }

    /// `maybe_new_socket(handle, domain, flags)`: a new descriptor of
    /// `family` if the handle has none (libuv never passes `UV_HANDLE_BOUND`
    /// here on Linux).
    fn maybe_new_socket(
        &mut self,
        family: AddressFamily,
        readable: bool,
        writable: bool,
    ) -> Result<(), i32> {
        if self.fd.is_none() {
            let fd = new_fd(family)?;
            // `new_socket`: on failure the descriptor is closed (dropped)
            return self.stream_open(fd, readable, writable);
        }
        self.readable |= readable;
        self.writable |= writable;
        Ok(())
    }

    /// `uv__write_req_update`: `n` more bytes of the head request were
    /// written; whether all of it is.
    fn write_req_update(&mut self, n: usize) -> bool {
        self.write_queue_size -= n;
        let req = self.write_queue.front_mut().expect("a request");
        let data = &req.data;
        let (index, offset) = advance(
            &|i| data.get(i).len(),
            data.count(),
            req.index,
            req.offset,
            n,
        );
        req.index = index;
        req.offset = offset;
        req.index == req.data.count()
    }

    /// `uv__write_req_finish`: the head request goes to the completed queue,
    /// whose callbacks run at the loop's next turn.
    fn write_req_finish(&mut self) {
        let mut req = self.write_queue.pop_front().expect("a request");
        if req.error == 0 {
            req.freed = true;
        }
        self.write_completed.push_back(req);
        self.feed();
    }

    /// `uv__try_write` of the head request: the bytes written, or libuv's
    /// code (`UV_EAGAIN` for a full socket).
    fn try_write(&self) -> Result<usize, i32> {
        let req = self.write_queue.front().expect("a request");
        let fd = self.fd().expect("a writable socket has a descriptor");
        let count = req.data.count();
        let mut iov: Vec<IoSlice<'_>> = Vec::with_capacity((count - req.index).min(IOV_MAX));
        for i in req.index..count {
            if iov.len() == IOV_MAX {
                break;
            }
            let b = req.data.get(i);
            let b = if i == req.index { &b[req.offset..] } else { b };
            iov.push(IoSlice::new(b));
        }
        loop {
            // `uv__writev`: write(2) for one buffer, writev(2) for more
            let r = if iov.len() == 1 {
                rustix::io::write(fd, &iov[0])
            } else {
                rustix::io::writev(fd, &iov)
            };
            match r {
                Ok(n) => return Ok(n),
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) | Err(Errno::NOBUFS) => return Err(UV_EAGAIN),
                Err(e) => return Err(uv_err(e)),
            }
        }
    }

    /// `uv__write`: write the queued requests in order, at most 33 of them,
    /// until the socket is full (then wait for `POLLOUT`) or a write fails.
    fn write(&mut self) {
        let mut count = 32;
        loop {
            if self.write_queue.is_empty() {
                return;
            }
            match self.try_write() {
                Ok(n) => {
                    if self.write_req_update(n) {
                        self.write_req_finish();
                        if count > 0 {
                            count -= 1;
                            continue;
                        }
                        return;
                    }
                }
                Err(UV_EAGAIN) => {}
                Err(e) => {
                    self.write_queue.front_mut().expect("a request").error = e;
                    self.write_req_finish();
                    self.io_stop(false, true);
                    return;
                }
            }
            self.io_start(false, true);
            return;
        }
    }

    /// `uv__stream_flush_write_queue(stream, error)`.
    fn flush_write_queue(&mut self, error: i32) {
        while let Some(mut req) = self.write_queue.pop_front() {
            req.error = error;
            self.write_completed.push_back(req);
        }
    }
}

#[cfg(test)]
impl TcpSocket {
    /// A `uv__io_feed` now, with nothing pending: the state a write that
    /// finished on the loop leaves behind (the unit tests').
    pub(crate) fn feed_for_tests(&self) {
        self.0.borrow_mut().feed();
    }
}

impl TcpSocket {
    /// `Socket.new` (`lean_uv_tcp_new`, `uv_tcp_init`): a socket with no
    /// descriptor yet. The first stream also takes libuv's spare descriptor
    /// (`/dev/null`, kept open), as natively.
    pub fn new() -> Result<TcpSocket, IoError> {
        crate::sched::ensure_started();
        crate::io::effect_point();
        super::loop_lock();
        Ok(TcpSocket::make())
    }

    fn make() -> TcpSocket {
        reserve_emfile_fd();
        TcpSocket(Rc::new(Handle::new(|id| {
            let mut t = Tcp::default();
            t.id = id;
            t
        })))
    }
}

// ---------------------------------------------------------------------------
// The loop's side (`uv__stream_io`, `uv__server_io`)

/// A socket's state as a loop callback sees it ([`super::on_socket`]).
struct Loop<'a>(&'a RefCell<Tcp>);

impl Loop<'_> {
    /// The watch callback and `uv__io_feed`'s call.
    fn on_io(s: &RefCell<Tcp>, ev: Ev) {
        let t = Loop(s);
        if s.borrow().listening {
            t.server_io();
        } else {
            t.stream_io(ev);
        }
    }

    /// `uv__stream_io`.
    fn stream_io(&self, ev: Ev) {
        {
            let t = self.0.borrow();
            if t.connect.is_some() {
                drop(t);
                self.stream_connect();
                return;
            }
            if t.fd.is_none() {
                return;
            }
        }
        if ev.read {
            self.read_ready();
        }
        if ev.hup {
            // `POLLHUP` after a partial read, with a receive started again
            // meanwhile: end of file, without reading
            let r = {
                let mut t = self.0.borrow_mut();
                if t.read.is_some() && t.read_partial && !t.read_eof {
                    t.read_eof = true;
                    t.io_stop(true, false);
                    t.read.take()
                } else {
                    None
                }
            };
            if let Some((r, keep)) = r {
                finish_read(r, ReadOutcome::Eof);
                drop(keep);
            }
        }
        if ev.write {
            self.0.borrow_mut().write();
            self.write_callbacks();
            let empty = self.0.borrow().write_queue.is_empty();
            if empty {
                self.drain();
            }
        }
    }

    /// `uv__read`, with Lean's read callbacks (which stop reading first,
    /// `uv_read_stop`): one read per callback; after a read that filled the
    /// buffer, libuv reads on if the callback started another receive.
    fn read_ready(&self) {
        let mut count = 32;
        self.0.borrow_mut().read_partial = false;
        loop {
            let mut t = self.0.borrow_mut();
            if t.read.is_none() || count == 0 {
                return;
            }
            count -= 1;
            let fd = t.fd.clone().expect("a reading socket has a descriptor");
            let (req, _) = t.read.as_mut().expect("checked");
            let zero_buf = match req {
                ReadReq::Recv(op) => op.target().len() == 0,
                ReadReq::Wait(_) => false,
            };
            // the allocation callback: Lean's buffer (its capacity), or none
            // for `waitReadable`; a buffer of 0 bytes is `UV_ENOBUFS`
            let (outcome, full) = match req {
                ReadReq::Wait(_) => (ReadOutcome::NoBufs, false),
                ReadReq::Recv(op) => {
                    let target = op.target();
                    let buflen = target.len();
                    if buflen == 0 {
                        (zero_size_outcome(&fd), false)
                    } else {
                        match super::recv_into(fd.as_fd(), target, false) {
                            Ok((0, _)) => (ReadOutcome::Eof, false),
                            Ok((n, _)) => (ReadOutcome::Data(n), n == buflen),
                            // nothing to read after all: libuv calls back
                            // with 0 bytes, which Lean resolves as an empty
                            // array
                            Err(Errno::AGAIN) => (ReadOutcome::Data(0), false),
                            Err(e) => (ReadOutcome::Error(uv_err(e)), false),
                        }
                    }
                }
            };
            let partial = matches!(outcome, ReadOutcome::Data(n) if n > 0) && !full;
            match &outcome {
                ReadOutcome::Error(_) => {
                    // libuv clears both flags before the callback
                    t.readable = false;
                    t.writable = false;
                }
                // `uv__stream_eof` (a read of 0 bytes; not LB-26's answer for
                // a buffer of 0 bytes, which reads nothing)
                ReadOutcome::Eof if !zero_buf => {
                    t.read_eof = true;
                }
                _ => {}
            }
            // Lean's callback: `uv_read_stop`, then resolve
            t.io_stop(true, false);
            let (req, keep) = t.read.take().expect("checked");
            drop(t);
            finish_read(req, outcome);
            drop(keep);
            if partial {
                // no more data to read for now
                self.0.borrow_mut().read_partial = true;
                return;
            }
            if !full {
                return;
            }
        }
    }

    /// `uv__write_callbacks`: the completed writes' promises, in order.
    fn write_callbacks(&self) {
        let reqs: VecDeque<WriteReq> = std::mem::take(&mut self.0.borrow_mut().write_completed);
        for req in reqs {
            if !req.freed {
                let r = req.remaining();
                self.0.borrow_mut().write_queue_size -= r;
            }
            let WriteReq {
                data,
                error,
                done,
                _keep,
                ..
            } = req;
            // Lean's write callback: resolve, then let go of the data and
            // the socket
            done(with_code(error));
            drop(data);
            drop(_keep);
        }
    }

    /// `uv__drain`: the write queue is empty; a pending shutdown happens now.
    fn drain(&self) {
        let req = {
            let mut t = self.0.borrow_mut();
            t.io_stop(false, true);
            match t.shutdown.take() {
                Some(req) => {
                    let fd = t.fd.clone().expect("a shut socket has a descriptor");
                    let err = match rustix::net::shutdown(&*fd, rustix::net::Shutdown::Write) {
                        Ok(()) => {
                            t.shut = true;
                            0
                        }
                        Err(e) => uv_err(e),
                    };
                    Some((req, err))
                }
                None => None,
            }
        };
        if let Some((req, err)) = req {
            (req.done)(with_code(err));
            drop(req._keep);
        }
    }

    /// `uv__stream_connect`: the outcome of a connect (the delayed error, or
    /// `SO_ERROR`).
    fn stream_connect(&self) {
        let (req, error) = {
            let mut t = self.0.borrow_mut();
            let error = if t.delayed_error != 0 {
                std::mem::replace(&mut t.delayed_error, 0)
            } else {
                match t.fd() {
                    Some(fd) => match sockopt::socket_error(fd) {
                        Ok(Ok(())) => 0,
                        Ok(Err(e)) => uv_err(e),
                        Err(e) => uv_err(e),
                    },
                    None => UV_EBADF,
                }
            };
            if error == UV_EINPROGRESS {
                return;
            }
            let req = t.connect.take().expect("a connect");
            if error < 0 || t.write_queue.is_empty() {
                t.io_stop(false, true);
            }
            (req, error)
        };
        // Lean's connect callback
        (req.done)(with_code(error));
        drop(req._keep);
        if error < 0 {
            self.0.borrow_mut().flush_write_queue(UV_ECANCELED);
            self.write_callbacks();
            // LEAN-BUG LB-28: a shutdown requested while the connect was
            // pending stays pending natively, forever; here it fails as the
            // writes queued behind the connect do.
            let sh = self.0.borrow_mut().shutdown.take();
            if let Some(sh) = sh {
                (sh.done)(with_code(UV_ECANCELED));
                drop(sh._keep);
            }
        } else {
            // LEAN-BUG LB-28: libuv stops `POLLOUT` here when no write is
            // queued and never reaches `uv__drain`, so a shutdown requested
            // while the connect was pending never happens (no FIN, the
            // promise never resolves, the socket is kept). Here it happens
            // now, as it does behind a queued write.
            let go = {
                let t = self.0.borrow();
                t.shutdown.is_some() && t.write_queue.is_empty()
            };
            if go {
                self.drain();
            }
        }
    }

    /// `uv__server_io`: accept one connection; Lean's connection callback
    /// hands it to a pending `accept`, otherwise the loop keeps it and stops
    /// watching until an `accept` takes it.
    fn server_io(&self) {
        let done = {
            let mut t = self.0.borrow_mut();
            if t.accepted_fd.is_some() {
                return;
            }
            let Some(fd) = t.fd.clone() else {
                return;
            };
            let r = match accept_fd(&fd) {
                Err(UV_EMFILE) | Err(UV_ENFILE) => Err(emfile_trick(&fd)),
                r => r,
            };
            let Ok(c) = r else {
                return;
            };
            t.accepted_fd = Some(c);
            // Lean's connection callback (`lean_uv_tcp_listen`)
            let done = t
                .accept
                .take()
                .map(|req| (req, t.uv_accept().map_err(uv_error)));
            if t.accepted_fd.is_some() {
                t.io_stop(true, false);
            }
            done
        };
        if let Some((req, r)) = done {
            (req.done)(r);
            drop(req._keep);
        }
    }
}

// ---------------------------------------------------------------------------
// The externs

impl TcpSocket {
    /// `Socket.connect` (`lean_uv_tcp_connect`, `uv_tcp_connect`): start
    /// connecting to `addr`; `done` gets the outcome on the loop.
    pub fn connect(
        &self,
        addr: SocketAddr,
        done: impl FnOnce(Result<(), IoError>) + 'static,
    ) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        if t.connect.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        if t.delayed_error == 0 {
            t.maybe_new_socket(family_of(&addr), true, true)
                .map_err(uv_error)?;
            let mut a = sockaddr(&addr);
            if let SocketAddr::V6(v6) = addr {
                let o = v6.ip().octets();
                if o[0] == 0xfe && o[1] == 0x80 {
                    a = rustix::net::SocketAddrAny::from(std::net::SocketAddrV6::new(
                        *v6.ip(),
                        v6.port(),
                        0,
                        link_local_scope_id(),
                    ));
                }
            }
            let fd = t.fd.clone().expect("made above");
            let r = loop {
                match rustix::net::connect(&*fd, &a) {
                    Err(Errno::INTR) => continue,
                    r => break r,
                }
            };
            match r {
                Ok(()) | Err(Errno::INPROGRESS) => {}
                // reported at the loop's next turn, as on other systems
                Err(Errno::CONNREFUSED) => t.delayed_error = UV_ECONNREFUSED,
                Err(e) => return Err(uv_error(uv_err(e))),
            }
        }
        t.connect = Some(Pending {
            done: Box::new(done),
            _keep: self.clone(),
        });
        t.io_start(false, true);
        if t.delayed_error != 0 {
            t.feed();
        }
        Ok(())
    }

    /// `Socket.send` (`lean_uv_tcp_send`, `uv_write`): queue `data`, writing
    /// at once what the socket takes; `done` gets the outcome on the loop
    /// once all of it is written. An empty array resolves at once.
    pub fn send<D: SendData>(
        &self,
        data: D,
        done: impl FnOnce(Result<(), IoError>) + 'static,
    ) -> Result<(), IoError> {
        crate::io::effect_point();
        if data.count() == 0 {
            done(Ok(()));
            return Ok(());
        }
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        // `uv__check_before_write`
        if t.fd.is_none() {
            return Err(uv_error(UV_EBADF));
        }
        if !t.writable {
            return Err(uv_error(UV_EPIPE));
        }
        let empty_queue = t.write_queue_size == 0;
        let req = WriteReq {
            data: Box::new(data),
            index: 0,
            offset: 0,
            error: 0,
            freed: false,
            done: Box::new(done),
            _keep: self.clone(),
        };
        t.write_queue_size += req.remaining();
        t.write_queue.push_back(req);
        if t.connect.is_some() {
            // still connecting
        } else if empty_queue {
            t.write();
        } else {
            t.io_start(false, true);
        }
        Ok(())
    }

    /// `Socket.recv?` (`lean_uv_tcp_recv`, `uv_read_start`): `alloc` makes
    /// the translator's `ByteArray` of the size asked for (Lean's
    /// `lean_alloc_sarray(1, 0, size)`), after the check for a receive in
    /// progress; `done` gets the bytes and their count (`some`), end of file
    /// (`none`) or an error, on the loop. A size of 0 fails with `ENOBUFS`
    /// once the socket is readable.
    pub fn recv<B: RecvBuf>(
        &self,
        alloc: impl FnOnce() -> B,
        done: impl FnOnce(Result<Option<(B, usize)>, IoError>) + 'static,
    ) -> Result<(), IoError> {
        super::loop_lock();
        if self.0.borrow().read.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        // Lean's allocation of the buffer, at the call (`lean_alloc_sarray`,
        // with its internal panics for an impossible size), with the socket
        // not borrowed: the glue's panic path is an effect point, where
        // another context may use the socket (review RNET-01).
        // A size of 0 waits for the socket to be readable, then fails with
        // `ENOBUFS` while bytes are unread, and gives `none` at the end of
        // the stream (LB-26; `zero_size_outcome`).
        let buf = alloc();
        let mut t = self.0.borrow_mut();
        if t.read.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        Self::read_start(&mut t)?;
        let op: Box<dyn RecvOp> = Box::new(Recv {
            buf,
            done: Box::new(done),
        });
        t.read = Some((ReadReq::Recv(op), self.clone()));
        Ok(())
    }

    /// `Socket.waitReadable` (`lean_uv_tcp_wait_readable`): `done` gets
    /// `true` once the socket is readable, end of file included (libuv's
    /// read callback reports `UV_ENOBUFS` for Lean's empty buffer, so the
    /// end of file is never read here).
    pub fn wait_readable(
        &self,
        done: impl FnOnce(Result<bool, IoError>) + 'static,
    ) -> Result<(), IoError> {
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        if t.read.is_some() {
            return Err(uv_error(UV_EALREADY));
        }
        Self::read_start(&mut t)?;
        t.read = Some((ReadReq::Wait(Box::new(done)), self.clone()));
        Ok(())
    }

    /// `uv_read_start`'s checks and start.
    fn read_start(t: &mut Tcp) -> Result<(), IoError> {
        if !t.readable {
            return Err(uv_error(UV_ENOTCONN));
        }
        if t.fd.is_none() {
            // libuv asserts a descriptor; a readable handle has one
            return Err(uv_error(UV_EBADF));
        }
        t.read_eof = false;
        t.io_start(true, false);
        Ok(())
    }

    /// `Socket.cancelRecv` (`lean_uv_tcp_cancel_recv`): stop a pending
    /// receive and let go of its promise without resolving it.
    pub fn cancel_recv(&self) {
        super::loop_lock();
        let r = {
            let mut t = self.0.borrow_mut();
            if t.read.is_none() {
                return;
            }
            t.io_stop(true, false);
            t.read.take()
        };
        // dropped outside the borrow: the promise's last reference resolves
        // it with `none`, which may run its `sync` dependents
        drop(r);
    }

    /// `Socket.bind` (`lean_uv_tcp_bind`, `uv_tcp_bind(handle, addr, 0)`).
    pub fn bind(&self, addr: SocketAddr) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        t.maybe_new_socket(family_of(&addr), false, false)
            .map_err(uv_error)?;
        let fd = t.fd.clone().expect("made above");
        sockopt::set_socket_reuseaddr(&*fd, true).map_err(|e| uv_error(uv_err(e)))?;
        if addr.is_ipv6() {
            sockopt::set_ipv6_v6only(&*fd, false).map_err(|e| uv_error(uv_err(e)))?;
        }
        match rustix::net::bind(&*fd, &sockaddr(&addr)) {
            Ok(()) => t.delayed_error = 0,
            Err(Errno::ADDRINUSE) => t.delayed_error = UV_EADDRINUSE,
            Err(Errno::AFNOSUPPORT) => return Err(uv_error(UV_EINVAL)),
            Err(e) => return Err(uv_error(uv_err(e))),
        }
        t.bound = true;
        if addr.is_ipv6() {
            t.ipv6 = true;
        }
        Ok(())
    }

    /// `Socket.listen` (`lean_uv_tcp_listen`, `uv_listen`): Lean's `UInt32`
    /// backlog is passed on as a C `int`. A socket without a descriptor gets
    /// an IPv4 one (the kernel binds it to a free port).
    pub fn listen(&self, backlog: u32) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        if t.delayed_error != 0 {
            return Err(uv_error(t.delayed_error));
        }
        t.maybe_new_socket(AddressFamily::INET, false, false)
            .map_err(uv_error)?;
        let fd = t.fd.clone().expect("made above");
        rustix::net::listen(&*fd, backlog as i32).map_err(|e| uv_error(uv_err(e)))?;
        t.listening = true;
        t.bound = true;
        if t.accepted_fd.is_none() {
            t.io_start(true, false);
        }
        Ok(())
    }

    /// `Socket.accept` (`lean_uv_tcp_accept`): a connection the loop already
    /// accepted resolves `done` at once; otherwise the next one does, on the
    /// loop. A second `accept` while one is pending fails with `EALREADY`.
    pub fn accept(
        &self,
        done: impl FnOnce(Result<TcpSocket, IoError>) + 'static,
    ) -> Result<(), IoError> {
        super::loop_lock();
        let r = {
            let mut t = self.0.borrow_mut();
            if t.accept.is_some() {
                // LEAN-BUG LB-21: native returns this error with the event
                // loop still locked (no `event_loop_unlock`), so no loop
                // callback ever runs again; here the loop goes on.
                return Err(parallel_accept());
            }
            match t.uv_accept() {
                Err(UV_EAGAIN) => {
                    t.accept = Some(Pending {
                        done: Box::new(done),
                        _keep: self.clone(),
                    });
                    return Ok(());
                }
                r => r.map_err(uv_error),
            }
        };
        done(r);
        Ok(())
    }

    /// `Socket.tryAccept` (`lean_uv_tcp_try_accept`): the connection the loop
    /// accepted, if any.
    pub fn try_accept(&self) -> Result<Option<TcpSocket>, IoError> {
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        if t.accept.is_some() {
            return Err(parallel_accept());
        }
        match t.uv_accept() {
            Ok(c) => Ok(Some(c)),
            Err(UV_EAGAIN) => Ok(None),
            Err(e) => Err(uv_error(e)),
        }
    }

    /// `Socket.cancelAccept` (`lean_uv_tcp_cancel_accept`): let go of a
    /// pending accept's promise without resolving it.
    pub fn cancel_accept(&self) {
        super::loop_lock();
        let r = self.0.borrow_mut().accept.take();
        drop(r);
    }

    /// `Socket.shutdown` (`lean_uv_tcp_shutdown`, `uv_shutdown`): no more
    /// sends from now on; the write side is shut once the queued writes are
    /// done, and `done` gets the outcome then, on the loop.
    pub fn shutdown(
        &self,
        done: impl FnOnce(Result<(), IoError>) + 'static,
    ) -> Result<(), IoError> {
        crate::io::effect_point();
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        if t.shutdown.is_some() {
            return Err(IoError::decode_uv_error(
                UV_EALREADY,
                Some(b"shutdown already in progress"),
            ));
        }
        if !t.writable || t.shut || t.fd.is_none() {
            // LEAN-BUG LB-25: native keeps a reference to the socket here,
            // so the socket and its descriptor are never freed; nothing is
            // taken before this check here.
            return Err(uv_error(UV_ENOTCONN));
        }
        t.shutdown = Some(Pending {
            done: Box::new(done),
            _keep: self.clone(),
        });
        t.writable = false;
        if t.write_queue.is_empty() {
            t.feed();
        }
        Ok(())
    }

    /// `uv__getsockpeername` after the delayed error.
    fn name(&self, peer: bool) -> Result<SocketAddr, IoError> {
        let t = self.0.borrow();
        if t.delayed_error != 0 {
            return Err(uv_error(t.delayed_error));
        }
        let fd = t.fd().ok_or_else(|| uv_error(UV_EBADF))?;
        let a = if peer {
            rustix::net::getpeername(fd).map_err(|e| uv_error(uv_err(e)))?
        } else {
            Some(rustix::net::getsockname(fd).map_err(|e| uv_error(uv_err(e)))?)
        };
        a.and_then(socket_address)
            .ok_or_else(|| uv_error(UV_EAFNOSUPPORT))
    }

    /// `Socket.getPeerName` (`lean_uv_tcp_getpeername`).
    pub fn peer_name(&self) -> Result<SocketAddr, IoError> {
        super::loop_lock();
        self.name(true)
    }

    /// `Socket.getSockName` (`lean_uv_tcp_getsockname`).
    pub fn sock_name(&self) -> Result<SocketAddr, IoError> {
        super::loop_lock();
        self.name(false)
    }

    /// `Socket.noDelay` (`lean_uv_tcp_nodelay`, `uv_tcp_nodelay(handle, 1)`):
    /// `TCP_NODELAY` now, or on the descriptor the socket gets later.
    pub fn no_delay(&self) -> Result<(), IoError> {
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        if let Some(fd) = t.fd() {
            sockopt::set_tcp_nodelay(fd, true).map_err(|e| uv_error(uv_err(e)))?;
        }
        t.nodelay = true;
        Ok(())
    }

    /// `Socket.keepAlive` (`lean_uv_tcp_keepalive`, `uv_tcp_keepalive`):
    /// Lean's `Int8` is a C `int` there, so any value but 0 turns keep-alive
    /// on. With a descriptor: `SO_KEEPALIVE`, then the idle time `delay`
    /// (s; 0 or more than the kernel's 32767 fails with `EINVAL`),
    /// a 1 s interval and 10 probes. Without one, the setting waits for the
    /// descriptor, with an idle time of 60 s (libuv's).
    pub fn keep_alive(&self, enable: i32, delay: u32) -> Result<(), IoError> {
        super::loop_lock();
        let mut t = self.0.borrow_mut();
        let on = enable != 0;
        if let Some(fd) = t.fd() {
            tcp_keepalive(fd, on, delay).map_err(uv_error)?;
        }
        t.keepalive = on;
        Ok(())
    }
}

/// `UV_EALREADY` with Lean's message (which `decode_uv_error` drops: the
/// class of `EALREADY` takes no file name).
fn parallel_accept() -> IoError {
    IoError::decode_uv_error(
        UV_EALREADY,
        Some(b"parallel accept is not allowed! consider binding multiple sockets to the same address and accepting on them instead"),
    )
}

impl Tcp {
    /// `uv_accept(server, client)` with a new client: the accepted
    /// descriptor as an open, bound stream; `UV_EAGAIN` if the loop has
    /// none.
    fn uv_accept(&mut self) -> Result<TcpSocket, i32> {
        let Some(fd) = self.accepted_fd.take() else {
            return Err(UV_EAGAIN);
        };
        let client = TcpSocket::make();
        {
            let mut c = client.0.borrow_mut();
            // a new handle has no flags, so this cannot fail
            c.stream_open(fd, true, true)?;
            c.bound = true;
        }
        self.io_start(true, false);
        Ok(client)
    }
}

/// `recv? 0` once the socket is readable. libuv calls back with
/// `UV_ENOBUFS` for Lean's buffer of 0 bytes without reading.
/// LEAN-BUG LB-26: at the end of the stream with no data left, Lean's
/// docstring says the result is `none`, where native gives `ENOBUFS`. So,
/// without consuming anything (a pending socket error stays for the next
/// receive): unread bytes (`FIONREAD`) are `ENOBUFS`, as natively; else the
/// peer's shutdown (`POLLRDHUP` or `POLLHUP`) without `POLLERR` is the end
/// of the stream; anything else is `ENOBUFS`.
fn zero_size_outcome(fd: &OwnedFd) -> ReadOutcome {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    match rustix::io::ioctl_fionread(fd) {
        Ok(0) => {}
        _ => return ReadOutcome::NoBufs,
    }
    let mut p = [PollFd::new(fd, PollFlags::IN | PollFlags::RDHUP)];
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if poll(&mut p, Some(&zero)).is_err() {
        return ReadOutcome::NoBufs;
    }
    let r = p[0].revents();
    if r.intersects(PollFlags::RDHUP | PollFlags::HUP) && !r.contains(PollFlags::ERR) {
        ReadOutcome::Eof
    } else {
        ReadOutcome::NoBufs
    }
}

fn finish_read(r: ReadReq, o: ReadOutcome) {
    match r {
        ReadReq::Recv(op) => op.finish(o),
        // `lean_uv_tcp_wait_readable`'s callback
        ReadReq::Wait(done) => done(match o {
            ReadOutcome::NoBufs => Ok(true),
            ReadOutcome::Eof => Ok(false),
            ReadOutcome::Error(e) => Err(uv_error(e)),
            // native asserts this never happens (no buffer, no read)
            ReadOutcome::Data(_) => Ok(true),
        }),
    }
}
