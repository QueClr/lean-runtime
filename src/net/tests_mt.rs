//! Unit tests of `net` in threads mode (docs/threads.md, 0.7): the paths
//! that cross threads. A socket's last handle dropped on another thread
//! while the loop thread has an operation pending, or while it watches the
//! socket, or with a `uv__io_feed` queued; a pending operation cancelled
//! from another thread, its promise (here the `done` closure) dropped
//! there uncalled; callbacks on the loop thread that call externs on their
//! own socket (the recursive loop lock); `accept`'s immediate resolution
//! after the loop lock; DNS answers resolved on the loop thread; externs
//! from several threads at once.
//!
//! The `done` closures run on the loop thread, where a failed assertion
//! would abort the process: they send what they saw to the test's thread,
//! which checks it. Each test takes `sched::mt`'s test lock: they share the
//! loop thread with each other and with `uv`'s tests (the loop thread's
//! count of socket callbacks, the loop lock's waiters). Loopback only.

use super::tcp::TcpSocket;
use super::tests::{
    bind_again, bound_outside_ephemeral, fd_open_to, full_listener, full_listener_on, received,
    socket_inode,
};
use super::udp::UdpSocket;
use super::*;
use crate::sched::mt::test_serial;
use crate::sched::uv;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::ThreadId;
use std::time::Duration;

/// How long a test waits for the loop thread at most.
const LONG: Duration = Duration::from_secs(10);

fn lo(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// What a `done` closure saw: its result's text, and whether it ran on the
/// loop thread.
type Seen = (String, bool);

/// A `done` closure that reports its result.
fn reporter<T: std::fmt::Debug>(tx: Sender<Seen>) -> impl FnOnce(Result<T, IoError>) + Send {
    move |r| {
        let _ = tx.send((format!("{r:?}"), uv::on_loop_thread()));
    }
}

/// A connected pair: a `TcpSocket` client and the peer's plain stream.
fn pair() -> (TcpSocket, TcpStream) {
    let l = TcpListener::bind(lo(0)).unwrap();
    let c = TcpSocket::new().unwrap();
    let (tx, rx) = channel();
    c.connect(l.local_addr().unwrap(), reporter::<()>(tx))
        .unwrap();
    let (peer, _) = l.accept().unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("Ok(())".into(), true));
    peer.set_read_timeout(Some(LONG)).unwrap();
    (c, peer)
}

/// What a TCP receive resolves with.
type Received = Result<Option<(Vec<u8>, usize)>, IoError>;

/// A receive's `done` that reports the bytes read.
fn bytes_reporter(tx: Sender<Seen>) -> impl FnOnce(Received) + Send {
    move |r| {
        let _ = tx.send((received(r), uv::on_loop_thread()));
    }
}

/// A `done` closure that reports its result after `tag`, so that the
/// operations of one socket can share a channel, which keeps their order.
fn tagged<T: std::fmt::Debug>(
    tag: &'static str,
    tx: Sender<Seen>,
) -> impl FnOnce(Result<T, IoError>) + Send {
    move |r| {
        let _ = tx.send((format!("{tag} {r:?}"), uv::on_loop_thread()));
    }
}

/// What a probe saw: what happened, on which thread, and whether that
/// thread held the loop lock.
type Probed = (&'static str, ThreadId, bool);

/// Dropped, it says so with the dropping thread and whether it held the
/// loop lock (a promise's `lean_dec`).
struct Probe(Option<Sender<Probed>>);

impl Drop for Probe {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(("dropped", std::thread::current().id(), uv::lock_held_here()));
        }
    }
}

/// How many loop callbacks found their socket so far (`net::CALLBACKS_RUN`,
/// the loop thread's), read on the loop thread through its async queue.
fn loop_callbacks_run() -> u32 {
    let (tx, rx) = channel();
    uv::post(Box::new(move || {
        let _ = tx.send(CALLBACKS_RUN.with(|n| n.get()));
    }));
    rx.recv_timeout(LONG)
        .expect("the loop thread runs the async queue")
}

/// The peer reads the end of the stream: the socket's descriptor is closed.
fn peer_reads_eof(peer: &mut TcpStream) {
    assert_eq!(peer.read(&mut [0; 8]).unwrap(), 0, "the socket closed");
}

/// The program drops its last handle on another thread while a receive is
/// pending: the receive holds the socket (native's `lean_inc(socket)`), so
/// it stays open and the loop thread completes the receive; its promise
/// resolves there; then the receive's reference was the last, and the
/// socket closes.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_socket_dropped_on_another_thread_while_a_read_is_pending() {
    let _s = test_serial();
    let (c, mut peer) = pair();
    let (tx, rx) = channel();
    c.recv(|| Vec::with_capacity(16), bytes_reporter(tx))
        .unwrap();
    std::thread::spawn(move || drop(c)).join().unwrap();
    peer.write_all(b"ping").unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("some ping".into(), true));
    peer_reads_eof(&mut peer);
}

/// `cancelRecv` on another thread while the loop thread waits for the
/// socket: the pending receive's `done` is dropped uncalled on the
/// cancelling thread, with the loop lock held (native's `lean_dec(promise)`
/// in `lean_uv_tcp_cancel_recv`, before `event_loop_unlock`); bytes that
/// come later wait for the next receive.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_cancel_recv_on_another_thread_drops_the_promise_there() {
    let _s = test_serial();
    let (c, mut peer) = pair();
    let (ptx, prx) = channel();
    let probe = Probe(Some(ptx.clone()));
    c.recv(
        || Vec::with_capacity(16),
        move |_: Received| {
            let _keep = &probe;
            let _ = ptx.send(("called", std::thread::current().id(), true));
        },
    )
    .unwrap();
    assert!(c.reading_for_tests());
    let c2 = c.clone();
    let by = std::thread::spawn(move || {
        c2.cancel_recv();
        std::thread::current().id()
    })
    .join()
    .unwrap();
    assert_eq!(prx.recv_timeout(LONG).unwrap(), ("dropped", by, true));
    assert!(!c.reading_for_tests());
    peer.write_all(b"late").unwrap();
    let (tx, rx) = channel();
    c.recv(|| Vec::with_capacity(16), bytes_reporter(tx))
        .unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("some late".into(), true));
    assert!(prx.try_recv().is_err(), "the cancelled receive never ran");
    drop(c);
    peer_reads_eof(&mut peer);
}

/// A connect whose socket the program dropped on another thread: it
/// completes on the loop thread (in either order of the drop and the
/// connection), its promise resolves there once, and then the socket
/// closes.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_connect_completes_after_the_program_dropped_its_socket() {
    let _s = test_serial();
    let l = TcpListener::bind(lo(0)).unwrap();
    let c = TcpSocket::new().unwrap();
    let (tx, rx) = channel();
    c.connect(l.local_addr().unwrap(), reporter::<()>(tx))
        .unwrap();
    std::thread::spawn(move || drop(c)).join().unwrap();
    let (mut peer, _) = l.accept().unwrap();
    peer.set_read_timeout(Some(LONG)).unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("Ok(())".into(), true));
    peer_reads_eof(&mut peer);
    assert!(rx.try_recv().is_err(), "resolved once");
}

/// AR-12 in threads mode: a listening socket with a connection waiting (its
/// descriptor ready, watched by the loop thread) dropped on another thread
/// closes its descriptor at once, under the loop lock (Lean's finalizer
/// takes it), and the port can be bound again at once. The test's thread
/// holds the loop lock while the connection comes and until the dropping
/// thread waits for the lock, so the loop thread cannot accept the
/// connection first: a waiter goes before the loop's next iteration.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_socket_dropped_while_watched_closes_at_once() {
    let _s = test_serial();
    let (t, addr) = bound_outside_ephemeral(|a| {
        let t = TcpSocket::new().ok()?;
        t.bind(a).ok()?;
        t.listen(4).ok()?;
        Some(t)
    });
    let inode = socket_inode("tcp", addr.port()).expect("the listening socket");
    assert!(fd_open_to(inode));
    let (dropper, _client) = {
        let _l = super::loop_lock();
        let client = TcpStream::connect(addr).unwrap();
        let waiters = uv::lock_waiters();
        let dropper = std::thread::spawn(move || drop(t));
        let deadline = std::time::Instant::now() + LONG;
        while uv::lock_waiters() == waiters {
            assert!(
                std::time::Instant::now() < deadline,
                "the drop waits for the loop lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        (dropper, client)
    };
    dropper.join().unwrap();
    assert!(!fd_open_to(inode), "the descriptor is closed at once");
    bind_again(|| TcpListener::bind(addr));
}

/// A socket dropped while a `uv__io_feed` of it is queued (the state a
/// write that finished on the loop leaves behind), with the loop lock held
/// so that the loop thread cannot run the feed first: the descriptor closes
/// at once; the feed then runs, finds no socket and does nothing, and the
/// loop goes on.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_feed_queued_after_the_socket_closed_runs_nothing() {
    let _s = test_serial();
    let (c, mut peer) = pair();
    let before = loop_callbacks_run();
    {
        let _l = super::loop_lock();
        c.feed_for_tests();
        drop(c);
        peer_reads_eof(&mut peer);
    }
    // UDP: a bound socket, on a port no parallel test's bind to port 0 can
    // take once it is free
    let (u, addr) = bound_outside_ephemeral(|a| {
        let u = UdpSocket::new().ok()?;
        u.bind(a).ok()?;
        Some(u)
    });
    let inode = socket_inode("udp", addr.port()).expect("the bound socket");
    {
        let _l = super::loop_lock();
        u.feed_for_tests();
        drop(u);
        assert!(!fd_open_to(inode), "the descriptor is closed at once");
    }
    // without `SO_REUSEADDR`: refused while the socket is open
    drop(bind_again(|| std::net::UdpSocket::bind(addr)));
    // the feeds ran at the start of the loop's next iteration, before the
    // async queue's callback that reads the count: they found no socket
    assert_eq!(
        loop_callbacks_run(),
        before,
        "no callback of the sockets ran"
    );
    // and the loop goes on
    let (c2, mut peer2) = pair();
    let (tx, rx) = channel();
    c2.recv(|| Vec::with_capacity(16), bytes_reporter(tx))
        .unwrap();
    peer2.write_all(b"after").unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("some after".into(), true));
}

/// A receive's `done` on the loop thread (the loop lock held there) starts
/// the next receive and sets an option on its own socket: the loop lock is
/// recursive, so they go on at once, as a `sync` dependent's externs do
/// natively; the last `done` lets go of the last handle there, and the
/// socket closes on the loop thread.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_done_on_the_loop_thread_calls_externs_on_its_socket() {
    let _s = test_serial();
    let (c, mut peer) = pair();
    let (tx, rx) = channel::<Seen>();
    let c2 = c.clone();
    let tx2 = tx.clone();
    c.recv(
        || Vec::with_capacity(4),
        move |r: Received| {
            let first = matches!(&r, Ok(Some((v, _))) if v.as_slice() == b"one");
            let again = c2.recv(|| Vec::with_capacity(4), bytes_reporter(tx2));
            let nodelay = c2.no_delay();
            let _ = tx.send((
                format!("{first} {} {}", again.is_ok(), nodelay.is_ok()),
                uv::on_loop_thread(),
            ));
        },
    )
    .unwrap();
    drop(c);
    peer.write_all(b"one").unwrap();
    assert_eq!(
        rx.recv_timeout(LONG).unwrap(),
        ("true true true".into(), true)
    );
    peer.write_all(b"two").unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("some two".into(), true));
    peer_reads_eof(&mut peer);
}

/// `accept` of a connection the loop already took resolves at once, after
/// the extern let go of the loop lock (native's `lean_uv_tcp_accept`
/// unlocks before `lean_promise_resolve`); one that waits resolves on the
/// loop thread.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_accept_resolves_after_the_loop_lock() {
    let _s = test_serial();
    let s = TcpSocket::new().unwrap();
    s.bind(lo(0)).unwrap();
    s.listen(4).unwrap();
    let addr = s.sock_name().unwrap();
    let deadline = std::time::Instant::now() + LONG;
    let mut clients = vec![TcpStream::connect(addr).unwrap()];
    loop {
        // the loop thread takes a connection as it arrives
        std::thread::sleep(Duration::from_millis(20));
        let (tx, rx) = channel();
        s.accept(move |r: Result<TcpSocket, IoError>| {
            let _ = tx.send((r.is_ok(), uv::lock_held_here(), uv::on_loop_thread()));
        })
        .unwrap();
        if let Ok(x) = rx.try_recv() {
            assert_eq!(x, (true, false, false), "at once, without the loop lock");
            break;
        }
        // not taken yet: the loop thread resolves it when it takes it
        let x = rx.recv_timeout(LONG).unwrap();
        assert_eq!(
            x,
            (true, true, true),
            "on the loop thread, with the loop lock"
        );
        assert!(std::time::Instant::now() < deadline);
        clients.push(TcpStream::connect(addr).unwrap());
    }
}

/// DNS in threads mode: the helper hands the answer to the loop thread,
/// which resolves the promise there (`localhost` from `/etc/hosts`).
#[test]
#[cfg_attr(miri, ignore)]
fn mt_dns_answers_resolve_on_the_loop_thread() {
    let _s = test_serial();
    let (tx, rx) = channel();
    dns::get_addr_info(
        "localhost",
        "",
        1,
        move |r: Result<Vec<IpAddr>, IoError>| {
            let has_lo = r
                .as_ref()
                .is_ok_and(|v| v.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
            let _ = tx.send((format!("{has_lo}"), uv::on_loop_thread()));
        },
    )
    .unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("true".into(), true));
    let (tx, rx) = channel();
    dns::get_name_info(lo(0), reporter::<(String, String)>(tx)).unwrap();
    let (r, on_loop) = rx.recv_timeout(LONG).unwrap();
    assert!(r.starts_with("Ok("), "{r}");
    assert!(on_loop);
}

/// A UDP socket's receive pending on the loop thread while the program
/// drops its handle on another thread: the receive holds the socket, gets
/// the next datagram on the loop thread, then lets go of the last handle,
/// and the port can be bound again. And `cancelRecv` on another thread
/// drops the receive's `done` there, uncalled.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_udp_receive_and_cancel_across_threads() {
    let _s = test_serial();
    let (u, addr) = bound_outside_ephemeral(|a| {
        let u = UdpSocket::new().ok()?;
        u.bind(a).ok()?;
        Some(u)
    });
    let (tx, rx) = channel();
    u.recv(
        || Vec::with_capacity(16),
        move |r: Result<udp::Datagram<Vec<u8>>, IoError>| {
            let shown = match r {
                Ok((v, _, from)) => format!("{} {}", String::from_utf8_lossy(&v), from.is_some()),
                Err(e) => format!("{e:?}"),
            };
            let _ = tx.send((shown, uv::on_loop_thread()));
        },
    )
    .unwrap();
    std::thread::spawn(move || drop(u)).join().unwrap();
    let sender = std::net::UdpSocket::bind(lo(0)).unwrap();
    sender.send_to(b"dgram", addr).unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("dgram true".into(), true));
    // without `SO_REUSEADDR`: refused while the socket is open
    let again = bind_again(|| std::net::UdpSocket::bind(addr));
    drop(again);
    // cancel from another thread
    let u = UdpSocket::new().unwrap();
    u.bind(lo(0)).unwrap();
    let (ptx, prx) = channel();
    let probe = Probe(Some(ptx.clone()));
    u.recv(
        || Vec::with_capacity(16),
        move |_: Result<udp::Datagram<Vec<u8>>, IoError>| {
            let _keep = &probe;
            let _ = ptx.send(("called", std::thread::current().id(), true));
        },
    )
    .unwrap();
    let u2 = u.clone();
    let by = std::thread::spawn(move || {
        u2.cancel_recv();
        std::thread::current().id()
    })
    .join()
    .unwrap();
    assert_eq!(prx.recv_timeout(LONG).unwrap(), ("dropped", by, true));
}

/// Externs from several threads at once, each on its own pair, while the
/// loop thread serves them all: every message arrives, in order, and every
/// promise resolves on the loop thread.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_externs_from_several_threads() {
    let _s = test_serial();
    let threads: Vec<_> = (0..4)
        .map(|k| {
            std::thread::spawn(move || {
                let (c, mut peer) = pair();
                for i in 0..20 {
                    let msg = format!("{k}:{i}");
                    let (tx, rx) = channel();
                    c.send(vec![msg.clone().into_bytes()], reporter::<()>(tx))
                        .unwrap();
                    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("Ok(())".into(), true));
                    let mut b = vec![0; msg.len()];
                    peer.read_exact(&mut b).unwrap();
                    assert_eq!(b, msg.as_bytes());
                    peer.write_all(msg.as_bytes()).unwrap();
                    let (tx, rx): (Sender<Seen>, Receiver<Seen>) = channel();
                    c.recv(|| Vec::with_capacity(64), bytes_reporter(tx))
                        .unwrap();
                    assert_eq!(
                        rx.recv_timeout(LONG).unwrap(),
                        (format!("some {msg}"), true)
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

/// RNT-01: within one iteration of the loop thread, libuv runs the io
/// watchers' callbacks first and the signal watchers last ("Run signal
/// watchers last", `uv__io_poll`, `linux.c`). A timer's callback on the loop
/// thread makes a socket readable and raises `SIGUSR1` on that thread, so
/// the next iteration finds both ready: the receive's promise resolves
/// before the signal watcher's (before the fix, the signal's came first).
/// In a child process: the signal's handler is the process's.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_io_callbacks_run_before_signals_in_an_iteration() {
    const CHILD: &str = "LEAN_RUNTIME_TEST_NET_ORDER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let _s = test_serial();
        let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "net::tests_mt::mt_io_callbacks_run_before_signals_in_an_iteration",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .expect("the child runs");
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child: {:?}", out.status);
        return;
    }
    use std::sync::Mutex;
    static EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    static PEER: Mutex<Option<TcpStream>> = Mutex::new(None);
    fn event(e: &'static str) {
        EVENTS.lock().unwrap().push(e);
    }
    #[derive(Clone)]
    struct SigP;
    impl uv::LoopPromise for SigP {
        fn is_resolved(&self) -> bool {
            false
        }
        fn resolve(&self, _: i64) {
            event("signal");
        }
    }
    #[derive(Clone)]
    struct TimerP;
    impl uv::LoopPromise for TimerP {
        fn is_resolved(&self) -> bool {
            false
        }
        fn resolve(&self, _: i64) {
            // on the loop thread, at the end of an iteration
            PEER.lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .write_all(b"x")
                .unwrap();
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGUSR1).unwrap();
            event("timer");
        }
    }
    let (c, peer) = pair();
    *PEER.lock().unwrap() = Some(peer);
    c.recv(
        || Vec::with_capacity(16),
        |r: Received| {
            event(if matches!(r, Ok(Some(_))) {
                "recv"
            } else {
                "recv failed"
            })
        },
    )
    .unwrap();
    // Lean's `SIGUSR1` is 10
    let sig: uv::Signal<SigP> = uv::Signal::new(10, false);
    let _sp = sig.next(|| SigP).unwrap();
    let t: uv::Timer<TimerP> = uv::Timer::new(50, false);
    let _tp = t.next(|| TimerP);
    let deadline = std::time::Instant::now() + LONG;
    while EVENTS.lock().unwrap().len() < 3 {
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            EVENTS.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*EVENTS.lock().unwrap(), ["timer", "recv", "signal"]);
}

/// RNT-03: once `sched::finish` has returned (the task manager's
/// finalization is over), the loop thread runs no callback, so no promise
/// resolves (natively one resolved then crashes the exit, LB-27; the
/// single-thread scheduler's loop context does not run after `finish`
/// either): neither a timer due then nor a receive whose bytes come then.
/// The externs of the exit still take the loop lock: a socket's last drop
/// closes it, and a new socket can be made. In a child process: `finish`
/// ends the process's task manager.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_no_callback_after_finish() {
    const CHILD: &str = "LEAN_RUNTIME_TEST_NET_FINISH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let _s = test_serial();
        let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "net::tests_mt::mt_no_callback_after_finish",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .expect("the child runs");
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child: {:?}", out.status);
        return;
    }
    use std::sync::Mutex;
    static EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    #[derive(Clone)]
    struct TimerP;
    impl uv::LoopPromise for TimerP {
        fn is_resolved(&self) -> bool {
            false
        }
        fn resolve(&self, _: i64) {
            EVENTS.lock().unwrap().push("timer");
        }
    }
    struct NoGlue;
    impl crate::sched::Glue for NoGlue {}
    crate::sched::start_with(std::sync::Arc::new(NoGlue), 2, 256 << 10);
    let (c, mut peer) = pair();
    c.recv(
        || Vec::with_capacity(16),
        |_: Received| EVENTS.lock().unwrap().push("recv"),
    )
    .unwrap();
    let t: uv::Timer<TimerP> = uv::Timer::new(100, false);
    let _tp = t.next(|| TimerP);
    crate::sched::finish();
    peer.write_all(b"x").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        EVENTS.lock().unwrap().is_empty(),
        "{:?}",
        EVENTS.lock().unwrap()
    );
    // the externs of the exit: the receive holds the socket, so its drop
    // does not close it; a new socket is made and closed
    drop(c);
    let n = TcpSocket::new().unwrap();
    n.bind(lo(0)).unwrap();
    drop(n);
    assert!(EVENTS.lock().unwrap().is_empty());
}

/// RNT-10: `finish` returns while the loop thread runs a callback (a
/// receive's `done`, a stand-in for a slow `sync` dependent, that waits
/// until `main`'s `finish` has returned); a 100 ms timer fell due
/// meanwhile. The running callback runs to its end, and the rest of that
/// iteration runs nothing: the timer's promise does not resolve (before the
/// fix, with the one check after the poll, it resolved after `finish`). In
/// a child process: `finish` ends the process's task manager.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_no_callback_after_finish_within_an_iteration() {
    const CHILD: &str = "LEAN_RUNTIME_TEST_NET_WINDOW_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let _s = test_serial();
        let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "net::tests_mt::mt_no_callback_after_finish_within_an_iteration",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .expect("the child runs");
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child: {:?}", out.status);
        return;
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static FINISHED: AtomicBool = AtomicBool::new(false);
    fn event(e: &str) {
        let after = FINISHED.load(Ordering::SeqCst);
        EVENTS
            .lock()
            .unwrap()
            .push(format!("{e} (finish returned: {after})"));
    }
    #[derive(Clone)]
    struct TimerP;
    impl uv::LoopPromise for TimerP {
        fn is_resolved(&self) -> bool {
            false
        }
        fn resolve(&self, _: i64) {
            event("timer");
        }
    }
    struct NoGlue;
    impl crate::sched::Glue for NoGlue {}
    crate::sched::start_with(std::sync::Arc::new(NoGlue), 2, 256 << 10);
    let (c, mut peer) = pair();
    c.recv(
        || Vec::with_capacity(16),
        |_: Received| {
            event("recv starts");
            // a slow `sync` dependent: until `main`'s `finish` has returned
            let deadline = std::time::Instant::now() + LONG;
            while !FINISHED.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            event("recv returns");
        },
    )
    .unwrap();
    let t: uv::Timer<TimerP> = uv::Timer::new(100, false);
    let _tp = t.next(|| TimerP);
    peer.write_all(b"x").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    crate::sched::finish();
    FINISHED.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        *EVENTS.lock().unwrap(),
        [
            "recv starts (finish returned: false)",
            "recv returns (finish returned: true)"
        ]
    );
}

/// LB-50 (hunt HN-01, HN-02) in threads mode: a `shutdown` requested while
/// the connect is really in progress (its SYN dropped by a full accept
/// queue) leaves the connect pending on the loop thread; once the
/// connection exists, the connect resolves `ok` there, then the shutdown,
/// which sends FIN.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_shutdown_during_a_slow_connect_waits_for_the_connection() {
    let _s = test_serial();
    let (l, _queue) = full_listener();
    let addr = l.local_addr().unwrap();
    let c = TcpSocket::new().unwrap();
    let (ctx, crx) = channel();
    c.connect(addr, reporter::<()>(ctx)).unwrap();
    let (stx, srx) = channel();
    c.shutdown(reporter::<()>(stx)).unwrap();
    // the shutdown's feed runs on the loop thread meanwhile
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        crx.try_recv().is_err(),
        "the connect waits for the connection"
    );
    assert!(
        srx.try_recv().is_err(),
        "the shutdown waits for the connect"
    );
    // free the queue: the SYN sent again is taken
    for _ in 0..2 {
        drop(l.accept().unwrap());
    }
    assert_eq!(crx.recv_timeout(LONG).unwrap(), ("Ok(())".into(), true));
    assert_eq!(srx.recv_timeout(LONG).unwrap(), ("Ok(())".into(), true));
    let (mut peer, _) = l.accept().unwrap();
    peer.set_read_timeout(Some(LONG)).unwrap();
    assert_eq!(peer.read(&mut [0; 8]).unwrap(), 0, "the shutdown sent FIN");
    assert_eq!(c.peer_name().unwrap(), addr);
}

/// LB-50's failure path in threads mode (review RF15-B01): the connect
/// stays pending while the SYN is dropped; then the listener and its queue
/// go away, so the SYN sent again gets a reset. On the loop thread the
/// connect resolves with `ECONNREFUSED`, then the shutdown queued behind it
/// with `ECANCELED` (LB-28). The listener's port is outside the ephemeral
/// range, so no parallel test's bind to port 0 can take it once closed.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_slow_connect_that_fails_fails_the_shutdown_behind_it() {
    let _s = test_serial();
    let ((l, queue), addr) = bound_outside_ephemeral(full_listener_on);
    let c = TcpSocket::new().unwrap();
    let (tx, rx) = channel();
    c.connect(addr, tagged::<()>("connect", tx.clone()))
        .unwrap();
    c.shutdown(tagged::<()>("shutdown", tx)).unwrap();
    // the shutdown's feed runs on the loop thread meanwhile
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rx.try_recv().is_err(),
        "the connect waits for the connection"
    );
    // close the listener: the SYN sent again gets a reset
    drop(queue);
    drop(l);
    assert_eq!(
        rx.recv_timeout(LONG).unwrap(),
        (format!("connect {:?}", with_code(UV_ECONNREFUSED)), true)
    );
    assert_eq!(
        rx.recv_timeout(LONG).unwrap(),
        (format!("shutdown {:?}", with_code(UV_ECANCELED)), true)
    );
    assert!(c.peer_name().is_err(), "no connection");
}

/// LB-50 with a receive pending, in threads mode (review RF15-B01): a
/// `recv?` started while the slow connect is pending, then a `shutdown`.
/// Once the connection exists, the connect resolves `ok` on the loop
/// thread, the shutdown sends FIN and resolves `ok`, the receive gets the
/// peer's bytes, and the peer reads the end of the stream.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_a_receive_pending_during_a_slow_connect_gets_the_bytes() {
    let _s = test_serial();
    let (l, _queue) = full_listener();
    let addr = l.local_addr().unwrap();
    let c = TcpSocket::new().unwrap();
    let (tx, rx) = channel();
    c.connect(addr, tagged::<()>("connect", tx.clone()))
        .unwrap();
    let rtx = tx.clone();
    c.recv(
        || Vec::with_capacity(16),
        move |r: Received| {
            let _ = rtx.send((format!("recv {}", received(r)), uv::on_loop_thread()));
        },
    )
    .unwrap();
    c.shutdown(tagged::<()>("shutdown", tx)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rx.try_recv().is_err(),
        "the connect waits for the connection"
    );
    // free the queue: the SYN sent again is taken
    for _ in 0..2 {
        drop(l.accept().unwrap());
    }
    let (mut peer, _) = l.accept().unwrap();
    peer.write_all(b"hi").unwrap();
    for want in ["connect Ok(())", "shutdown Ok(())", "recv some hi"] {
        assert_eq!(rx.recv_timeout(LONG).unwrap(), (want.into(), true));
    }
    peer.set_read_timeout(Some(LONG)).unwrap();
    assert_eq!(peer.read(&mut [0; 8]).unwrap(), 0, "the shutdown sent FIN");
}

/// LB-51 (hunt HN-03) in threads mode: `waitReadable` is `true` with bytes
/// unread (the end of the stream behind them), `false` at the end of the
/// stream with nothing left, and consumes nothing.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_wait_readable_is_false_at_the_end_of_the_stream() {
    let _s = test_serial();
    let (c, mut peer) = pair();
    peer.write_all(b"hi").unwrap();
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(wait_readable_of(&c), ("Ok(true)".into(), true));
    assert_eq!(recv_of(&c), ("some hi".into(), true));
    assert_eq!(wait_readable_of(&c), ("Ok(false)".into(), true));
    assert_eq!(wait_readable_of(&c), ("Ok(false)".into(), true));
    assert_eq!(recv_of(&c), ("none".into(), true));
}

/// `waitReadable` on `c`, waited for: its outcome's text, and whether it
/// resolved on the loop thread.
fn wait_readable_of(c: &TcpSocket) -> Seen {
    let (tx, rx) = channel();
    c.wait_readable(reporter::<bool>(tx)).unwrap();
    rx.recv_timeout(LONG).unwrap()
}

/// `recv? 16` on `c`, waited for, as [`wait_readable_of`].
fn recv_of(c: &TcpSocket) -> Seen {
    let (tx, rx) = channel();
    c.recv(|| Vec::with_capacity(16), bytes_reporter(tx))
        .unwrap();
    rx.recv_timeout(LONG).unwrap()
}

/// LB-51 in threads mode (review RF15-B02): after `recv?` has given
/// `none` (the peer's shutdown, nothing sent), `waitReadable` gives
/// `false`, as often as asked, and `recv?` still gives `none`.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_wait_readable_after_recv_gave_none_is_false() {
    let _s = test_serial();
    let (c, peer) = pair();
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(recv_of(&c), ("none".into(), true));
    assert_eq!(wait_readable_of(&c), ("Ok(false)".into(), true));
    assert_eq!(recv_of(&c), ("none".into(), true));
    assert_eq!(wait_readable_of(&c), ("Ok(false)".into(), true));
}

/// LB-51 in threads mode (review RF15-B02): a reset is not the end of the
/// stream. The peer closes with bytes it has not read, so its kernel sends
/// a reset: `waitReadable` gives `true` (the pending error, as natively),
/// again `true` (it consumes nothing), and `recv?` then fails with
/// `ECONNRESET`.
#[test]
#[cfg_attr(miri, ignore)]
fn mt_wait_readable_is_true_after_a_reset() {
    let _s = test_serial();
    let (c, peer) = pair();
    let (tx, rx) = channel();
    c.send(vec![b"unread".to_vec()], reporter::<()>(tx))
        .unwrap();
    assert_eq!(rx.recv_timeout(LONG).unwrap(), ("Ok(())".into(), true));
    // the bytes reach the peer's receive queue; closed with them unread,
    // its socket sends a reset
    std::thread::sleep(Duration::from_millis(50));
    drop(peer);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(wait_readable_of(&c), ("Ok(true)".into(), true));
    assert_eq!(wait_readable_of(&c), ("Ok(true)".into(), true));
    assert_eq!(
        recv_of(&c),
        (received(Err(uv_error(-crate::io::error::ECONNRESET))), true)
    );
}
