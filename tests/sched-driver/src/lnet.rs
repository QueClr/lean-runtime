//! A translator's glue around `lean_runtime::net`, as small as the cases
//! need: each extern makes the translator's promise (`lean_io_promise_new`),
//! hands the crate a closure that holds a reference to it (the loop's
//! `lean_inc`) and resolves it, and returns the promise; a receive allocates
//! the translator's `ByteArray` as Lean does (`lean_alloc_sarray`, with its
//! internal panics). And `Std.Async`'s `Async` monad, as its Lean code builds
//! tasks, for the cases written against `Std.Async.TCP` and `UDP`.

use crate::lean::{as_task, bind_task, has_finished, map_task, Task, UvPromise, PRIO_DEFAULT};
use crate::lio::R;
use lean_runtime::io::{exit, Handle, IoError};
use lean_runtime::net::tcp::TcpSocket;
use lean_runtime::net::udp::UdpSocket;
use lean_runtime::net::{dns, IpAddr, SocketAddr};
use lean_runtime::sched;
use lean_runtime::semantics::panic::InternalPanic;

/// `IO.Promise (Except IO.Error α)`, as the externs return it.
pub type P<T> = UvPromise<R<T>>;

/// A new promise, and the loop's reference to it as a resolving closure.
fn promise<T: Clone + 'static>() -> (P<T>, impl FnOnce(R<T>) + 'static) {
    let p: P<T> = UvPromise::new();
    let q = p.clone();
    (p, move |r| q.resolve(r))
}

/// `IO.wait p.result?`.
pub fn wait<T: Clone + 'static>(p: &P<T>) -> Option<R<T>> {
    p.result_opt().get()
}

/// `IO.wait p.result!` of a promise the program knows resolves.
pub fn wait_bang<T: Clone + 'static>(p: &P<T>) -> R<T> {
    wait(p).expect("a resolved promise")
}

/// `IO.Promise.isResolved` (`IO.hasFinished p.result?`).
pub fn is_resolved<T: Clone + 'static>(p: &P<T>) -> bool {
    has_finished(&p.result_opt())
}

/// `lean_internal_panic`: `INTERNAL PANIC: <msg>` on C's `stderr`, then
/// `exit(1)`.
pub fn internal_panic(p: InternalPanic) -> ! {
    sched::effect();
    let mut s = String::new();
    let _ = p.write_line(&mut s);
    let _ = Handle::stderr().put_str(s.as_bytes());
    exit::exit(1)
}

/// `lean_alloc_sarray(1, 0, size)`: Lean's checked size arithmetic, then the
/// allocation (reserved, not touched).
pub fn byte_array(size: u64) -> Vec<u8> {
    if let Err(p) = lean_runtime::semantics::array::alloc_bytes(1, size) {
        internal_panic(p)
    }
    let mut v = Vec::new();
    if v.try_reserve_exact(size as usize).is_err() {
        internal_panic(InternalPanic::OutOfMemory)
    }
    v
}

// ---------------------------------------------------------------------------
// TCP

pub fn tcp_connect(s: &TcpSocket, a: SocketAddr) -> R<P<()>> {
    let (p, done) = promise();
    s.connect(a, done)?;
    Ok(p)
}

pub fn tcp_send(s: &TcpSocket, bufs: Vec<Vec<u8>>) -> R<P<()>> {
    let (p, done) = promise();
    s.send(bufs, done)?;
    Ok(p)
}

pub fn tcp_recv(s: &TcpSocket, size: u64) -> R<P<Option<Vec<u8>>>> {
    let (p, done) = promise();
    s.recv(
        || byte_array(size),
        move |r| done(r.map(|o| o.map(|(v, _)| v))),
    )?;
    Ok(p)
}

pub fn tcp_wait_readable(s: &TcpSocket) -> R<P<bool>> {
    let (p, done) = promise();
    s.wait_readable(done)?;
    Ok(p)
}

pub fn tcp_accept(s: &TcpSocket) -> R<P<TcpSocket>> {
    let (p, done) = promise();
    s.accept(done)?;
    Ok(p)
}

pub fn tcp_shutdown(s: &TcpSocket) -> R<P<()>> {
    let (p, done) = promise();
    s.shutdown(done)?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// UDP

pub fn udp_send(s: &UdpSocket, bufs: Vec<Vec<u8>>, to: Option<SocketAddr>) -> R<P<()>> {
    let (p, done) = promise();
    s.send(bufs, to, done)?;
    Ok(p)
}

/// `(ByteArray × Option SocketAddress)`.
pub type Dgram = (Vec<u8>, Option<SocketAddr>);

pub fn udp_recv(s: &UdpSocket, size: u64) -> R<P<Dgram>> {
    let (p, done) = promise();
    s.recv(
        || byte_array(size),
        move |r| done(r.map(|(v, _, a)| (v, a))),
    )?;
    Ok(p)
}

pub fn udp_wait_readable(s: &UdpSocket) -> R<P<()>> {
    let (p, done) = promise();
    s.wait_readable(done)?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// DNS

pub fn get_addr_info(host: &str, service: &str, family: u8) -> R<P<Vec<IpAddr>>> {
    let (p, done) = promise();
    dns::get_addr_info(host, service, family, done)?;
    Ok(p)
}

pub fn get_name_info(a: SocketAddr) -> R<P<(String, String)>> {
    let (p, done) = promise();
    dns::get_name_info(a, done)?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// Addresses as Lean prints them (`IPv4Addr.toString`, `IPv6Addr.toString`:
// libuv's `uv_inet_ntop`), and `ofString`

pub fn ip_text(a: &IpAddr) -> String {
    let mut s = String::new();
    let _ = match a {
        IpAddr::V4(v4) => lean_runtime::semantics::net::ntop_v4(v4.octets(), &mut s),
        IpAddr::V6(v6) => lean_runtime::semantics::net::ntop_v6(v6.segments(), &mut s),
    };
    s
}

/// `(IPv4Addr.ofString s).get!`.
pub fn v4(s: &str) -> std::net::Ipv4Addr {
    let o = lean_runtime::semantics::net::pton_v4(s.as_bytes()).expect("an IPv4 address");
    std::net::Ipv4Addr::from(o)
}

/// `(IPv6Addr.ofString s).get!`.
pub fn v6(s: &str) -> std::net::Ipv6Addr {
    let g = lean_runtime::semantics::net::pton_v6(s.as_bytes()).expect("an IPv6 address");
    std::net::Ipv6Addr::new(g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7])
}

pub fn at4(ip: std::net::Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(ip), port)
}

pub fn at6(ip: std::net::Ipv6Addr, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V6(ip), port)
}

/// `String.fromUTF8? b`, shown as Lean's `toString` of an `Option String`.
pub fn utf8_opt(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => format!("(some {})", add_paren_heuristic(s)),
        Err(_) => "none".into(),
    }
}

/// Lean's `addParenHeuristic` (`Init/Data/ToString/Basic.lean`).
fn add_paren_heuristic(s: &str) -> String {
    if ["(", "[", "{", "#["].iter().any(|p| s.starts_with(p)) || !s.chars().any(char::is_whitespace)
    {
        s.to_string()
    } else {
        format!("({s})")
    }
}

// ---------------------------------------------------------------------------
// `Std.Async`'s `Async` (`EAsync IO.Error`): a `BaseIO` action giving a
// `MaybeTask`, a value or a task. Binds over a task are `BaseIO.bindTask`
// (an IO task), `ofPromise` maps the promise's `result?` (`Task.map`). The
// intermediate pure maps of `Async.map` are left out: they only add a pure
// task between two steps.

#[derive(Clone)]
pub enum Maybe<T: Clone + 'static> {
    Pure(R<T>),
    Task(Task<R<T>>),
}

impl<T: Clone + 'static> Maybe<T> {
    pub fn into_task(self) -> Task<R<T>> {
        match self {
            Maybe::Pure(r) => Task::pure(r),
            Maybe::Task(t) => t,
        }
    }
}

pub struct Async<T: Clone + 'static>(Box<dyn FnOnce() -> Maybe<T>>);

impl<T: Clone + 'static> Async<T> {
    pub fn new(f: impl FnOnce() -> Maybe<T> + 'static) -> Async<T> {
        Async(Box::new(f))
    }

    pub fn pure(v: T) -> Async<T> {
        Async::new(move || Maybe::Pure(Ok(v)))
    }

    /// `Async.ofPromise`: the extern runs when the `Async` does; its error is
    /// the `Async`'s.
    pub fn of_promise(io: impl FnOnce() -> R<P<T>> + 'static) -> Async<T> {
        Async::new(move || match io() {
            Err(e) => Maybe::Pure(Err(e)),
            Ok(p) => Maybe::Task(map_task(
                |o: Option<R<T>>| {
                    o.unwrap_or_else(|| {
                        Err(IoError::user_error(
                            "the promise linked to the Async was dropped",
                        ))
                    })
                },
                p.result_opt(),
                PRIO_DEFAULT,
                false,
                false,
            )),
        })
    }

    /// `EAsync.bind`.
    pub fn bind<U: Clone + 'static>(self, f: impl FnOnce(T) -> Async<U> + 'static) -> Async<U> {
        Async::new(move || match (self.0)() {
            Maybe::Pure(Ok(a)) => (f(a).0)(),
            Maybe::Pure(Err(e)) => Maybe::Pure(Err(e)),
            Maybe::Task(t) => Maybe::Task(bind_task(
                t,
                move |r| match r {
                    Ok(a) => (f(a).0)().into_task(),
                    Err(e) => Task::pure(Err(e)),
                },
                PRIO_DEFAULT,
                false,
                true,
            )),
        })
    }

    /// `Async.toIO` / `EAsync.toBaseIO`: run it here, the task of its result.
    pub fn start(self) -> Task<R<T>> {
        (self.0)().into_task()
    }

    /// `Async.block`: run it in a new task (`asTask`), wait, rethrow.
    pub fn block(self) -> R<T> {
        let t = as_task(move || (self.0)(), PRIO_DEFAULT);
        t.get().into_task().get()
    }
}
