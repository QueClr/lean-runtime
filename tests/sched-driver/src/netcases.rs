//! Rust ports of `tests/cases/net/*.lean`, line by line, over
//! `lean_runtime::net` with the glue of `lnet.rs`: the same externs in the
//! same order, each value dropped where compiled Lean releases it (a socket
//! closes when its last reference goes).

use crate::cases::Case;
use crate::glue::println;
use crate::lean::{as_task, sleep, PRIO_DEDICATED};
use crate::lio::{error_text, quote, R};
use crate::lnet::*;
use lean_runtime::io::{exit, IoError};
use lean_runtime::net::tcp::TcpSocket;
use lean_runtime::net::udp::UdpSocket;
use lean_runtime::net::{iface, IpAddr, SocketAddr};

pub fn lookup(id: &str) -> Option<Case> {
    fn no_init() {}
    Some(match id {
        "tcp_echo" => (no_init, tcp_echo),
        "tcp_errors" => (no_init, tcp_errors),
        "tcp_v6" => (no_init, tcp_v6),
        "udp_basic" => (no_init, udp_basic),
        "udp_errors" => (no_init, udp_errors),
        "dns_localhost" => (no_init, dns_localhost),
        "dns_pending_at_exit" => (no_init, dns_pending_at_exit),
        "iface_lo" => (no_init, iface_lo),
        "accept_parallel" => (no_init, accept_parallel),
        "accept_parallel_try" => (no_init, accept_parallel),
        "keepalive_zero_delay" => (no_init, keepalive_zero_delay),
        "multicast_ipv6_long" => (no_init, multicast_ipv6_long),
        "recv_huge_overflow" => (no_init, recv_huge),
        "recv_huge_oom" => (no_init, recv_huge),
        "udp_recv_huge_overflow" => (no_init, recv_huge),
        "udp_cancel_recv_leak" => (no_init, udp_cancel_recv_leak),
        "tcp_shutdown_fail_leak" => (no_init, tcp_shutdown_fail_leak),
        "recv_zero_eof" => (no_init, recv_zero),
        "recv_zero_data_eof" => (no_init, recv_zero),
        "recv_zero_data" => (no_init, recv_zero),
        "udp_recv_zero" => (no_init, recv_zero),
        "shutdown_during_connect" => (no_init, shutdown_connect),
        "shutdown_after_queued_write" => (no_init, shutdown_connect),
        "shutdown_after_connect" => (no_init, shutdown_connect),
        // Not cases: regression programs of net-1's review.
        "rnet_alloc_reentry" => (no_init, rnet_alloc_reentry),
        "rnet_shutdown_in_connect" => (no_init, rnet_shutdown_in_connect),
        _ => return None,
    })
}

/// A program's `main` ending with an uncaught error: Lean's message, status
/// 1 (`lean_io_result_show_error`).
fn run(r: R<()>) -> u32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            exit::show_error(error_text(&e).as_bytes());
            1
        }
    }
}

/// `try let s ← act; println s!"{name}: ok{s}" catch e => println s!"{name}: {e}"`.
fn try_io(name: &str, act: impl FnOnce() -> R<String>) {
    match act() {
        Ok(s) => println(&format!("{name}: ok{s}")),
        Err(e) => println(&format!("{name}: {}", error_text(&e))),
    }
}

/// `wait p f`: `, then <f v>`, `, then <e>`, or ` (dropped)`.
fn then<T: Clone + 'static>(p: &P<T>, f: impl FnOnce(T) -> String) -> String {
    match wait(p) {
        None => " (dropped)".into(),
        Some(Ok(v)) => format!(", then {}", f(v)),
        Some(Err(e)) => format!(", then {}", error_text(&e)),
    }
}

fn unit(_: ()) -> String {
    "ok".into()
}

fn bytes(b: Option<Vec<u8>>) -> String {
    match b {
        None => "none".into(),
        Some(b) => format!("some {}", std::str::from_utf8(&b).unwrap_or("?")),
    }
}

fn port(a: SocketAddr) -> u16 {
    a.port()
}

fn new_tcp() -> TcpSocket {
    TcpSocket::new().expect("a new socket")
}

fn new_udp() -> UdpSocket {
    UdpSocket::new().expect("a new socket")
}

// ---------------------------------------------------------------------------
// tcp_echo (Std.Async.TCP)

/// `str (b : Option ByteArray)`.
fn str_of(b: &Option<Vec<u8>>) -> String {
    match b {
        Some(b) => std::str::from_utf8(b).unwrap_or("<none>").to_string(),
        None => "<none>".into(),
    }
}

fn a_accept(s: &TcpSocket) -> Async<TcpSocket> {
    let s = s.clone();
    Async::of_promise(move || tcp_accept(&s))
}

fn a_recv(c: &TcpSocket, n: u64) -> Async<Option<Vec<u8>>> {
    let c = c.clone();
    Async::of_promise(move || tcp_recv(&c, n))
}

fn a_send(c: &TcpSocket, b: Vec<u8>) -> Async<()> {
    let c = c.clone();
    Async::of_promise(move || tcp_send(&c, vec![b]))
}

fn a_shutdown(c: &TcpSocket) -> Async<()> {
    let c = c.clone();
    Async::of_promise(move || tcp_shutdown(&c))
}

fn a_connect(c: &TcpSocket, a: SocketAddr) -> Async<()> {
    let c = c.clone();
    Async::of_promise(move || tcp_connect(&c, a))
}

/// `echoServer s n`.
fn echo_server(s: TcpSocket, n: usize) -> Async<()> {
    if n == 0 {
        return Async::pure(());
    }
    a_accept(&s).bind(move |c| {
        a_recv(&c, 1024).bind(move |m| {
            a_send(&c, format!("echo {}", str_of(&m)).into_bytes())
                .bind(move |_| a_shutdown(&c).bind(move |_| echo_server(s, n - 1)))
        })
    })
}

fn tcp_echo(args: &[String]) -> u32 {
    run((|| {
        let ip = v4(&args[0]);
        let s = new_tcp();
        s.bind(at4(ip, 0))?;
        s.listen(16)?;
        let port = s.sock_name()?.port();
        println(&format!("listening on a port: {}", port > 0));
        let st = echo_server(s.clone(), 2).start();
        for name in &args[1..] {
            let c = new_tcp();
            a_connect(&c, at4(ip, port)).start().get()?;
            println(&format!(
                "peer port matches: {}",
                c.peer_name()?.port() == port
            ));
            c.no_delay()?;
            a_send(&c, name.clone().into_bytes()).block()?;
            println(&format!(
                "client got {}",
                str_of(&a_recv(&c, 1024).block()?)
            ));
            println(&format!("then {}", str_of(&a_recv(&c, 1024).block()?)));
        }
        st.get()?;
        // blocking style
        let s2 = s.clone();
        let srv = as_task(
            move || -> R<()> {
                let c = a_accept(&s2).block()?;
                let m = a_recv(&c, 1024).block()?;
                println(&format!("server got {}", str_of(&m)));
                a_send(&c, b"pong".to_vec()).block()?;
                let m2 = a_recv(&c, 1024).block()?;
                println(&format!(
                    "server got {} after the client's shutdown",
                    str_of(&m2)
                ));
                Ok(())
            },
            PRIO_DEDICATED,
        );
        let c = new_tcp();
        a_connect(&c, at4(ip, port)).start().get()?;
        a_send(&c, b"ping".to_vec()).block()?;
        println(&format!(
            "client got {}",
            str_of(&a_recv(&c, 1024).block()?)
        ));
        a_shutdown(&c).block()?;
        srv.get()?;
        drop(c);
        // a big send, a shutdown behind it, a send after it
        let s3 = s.clone();
        let srv2 = as_task(
            move || -> R<u64> {
                let c = a_accept(&s3).block()?;
                sleep(50);
                let mut total = 0u64;
                loop {
                    match a_recv(&c, 65536).block()? {
                        None => break,
                        Some(b) => total += b.len() as u64,
                    }
                }
                Ok(total)
            },
            PRIO_DEDICATED,
        );
        let c4 = new_tcp();
        a_connect(&c4, at4(ip, port)).start().get()?;
        let big = vec![7u8; 16 * 1024 * 1024];
        let p1 = tcp_send(&c4, vec![big])?;
        let p2 = tcp_shutdown(&c4)?;
        match tcp_send(&c4, vec![b"x".to_vec()]) {
            Ok(p3) => println(&format!(
                "send after shutdown accepted: {}",
                wait_bang(&p3).is_ok()
            )),
            Err(e) => println(&format!("send after shutdown: {}", error_text(&e))),
        }
        println(&format!(
            "big send ok: {}, shutdown ok: {}",
            wait_bang(&p1).is_ok(),
            wait_bang(&p2).is_ok()
        ));
        match srv2.get() {
            Ok(n) => println(&format!("server received {n}")),
            Err(e) => println(&format!("server: {}", error_text(&e))),
        }
        match s.try_accept()? {
            None => println("tryAccept: none"),
            Some(_) => println("tryAccept: a socket?"),
        }
        println(&format!(
            "server still listening on the port: {}",
            s.sock_name()?.port() == port
        ));
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// tcp_errors

fn tcp_errors(args: &[String]) -> u32 {
    run((|| {
        let ip = v4(&args[0]);
        let at = |p| at4(ip, p);
        let u = new_tcp();
        try_io("getPeerName new", || u.peer_name().map(|_| String::new()));
        try_io("getSockName new", || u.sock_name().map(|_| String::new()));
        try_io("send new", || {
            tcp_send(&u, vec![vec![1]]).map(|_| String::new())
        });
        try_io("send new, no buffers", || {
            let p = tcp_send(&u, vec![])?;
            Ok(then(&p, unit))
        });
        try_io("recv? new", || tcp_recv(&u, 10).map(|_| String::new()));
        try_io("waitReadable new", || {
            tcp_wait_readable(&u).map(|_| String::new())
        });
        try_io("shutdown new", || tcp_shutdown(&u).map(|_| String::new()));
        try_io("tryAccept new", || {
            Ok(match u.try_accept() {
                Ok(None) => ", none".into(),
                Ok(Some(_)) => ", some".into(),
                Err(e) => format!(", {}", error_text(&e)),
            })
        });
        try_io("noDelay new", || u.no_delay().map(|_| String::new()));
        try_io("keepAlive 1 5 new", || {
            u.keep_alive(1, 5).map(|_| String::new())
        });
        try_io("cancelRecv new", || {
            u.cancel_recv();
            Ok(String::new())
        });
        try_io("cancelAccept new", || {
            u.cancel_accept();
            Ok(String::new())
        });
        // a bound socket
        let s = new_tcp();
        s.bind(at(0))?;
        try_io("keepAlive 1 40000", || {
            s.keep_alive(1, 40000).map(|_| String::new())
        });
        try_io("keepAlive 0 0", || {
            s.keep_alive(0, 0).map(|_| String::new())
        });
        try_io("keepAlive 1 5", || {
            s.keep_alive(1, 5).map(|_| String::new())
        });
        try_io("noDelay", || s.no_delay().map(|_| String::new()));
        try_io("getPeerName bound", || s.peer_name().map(|_| String::new()));
        try_io("bind again", || s.bind(at(0)).map(|_| String::new()));
        s.listen(16)?;
        let port = s.sock_name()?.port();
        try_io("recv? listening", || {
            tcp_recv(&s, 10).map(|_| String::new())
        });
        try_io("shutdown listening", || {
            tcp_shutdown(&s).map(|_| String::new())
        });
        try_io("send listening", || {
            tcp_send(&s, vec![vec![1]]).map(|_| String::new())
        });
        try_io("connect listening", || {
            let p = tcp_connect(&s, at(port))?;
            Ok(then(&p, unit))
        });
        // a client
        let c = new_tcp();
        let pc = tcp_connect(&c, at(port))?;
        println(&format!("connect{}", then(&pc, unit)));
        drop(pc);
        try_io("connect again", || {
            let p = tcp_connect(&c, at(port))?;
            Ok(then(&p, unit))
        });
        try_io("connect a third time", || {
            let p = tcp_connect(&c, at(port))?;
            Ok(then(&p, unit))
        });
        sleep(50);
        let pa = tcp_accept(&s)?;
        println(&format!(
            "accept of a connection the loop took: resolved at once {}",
            is_resolved(&pa)
        ));
        let sc = match wait(&pa) {
            Some(Ok(v)) => v,
            _ => return Err(IoError::user_error("accept failed")),
        };
        drop(pa);
        println(&format!(
            "peer of the accepted socket is the client: {}",
            sc.peer_name()?.port() == c.sock_name()?.port()
        ));
        try_io("waitReadable while a recv? is pending", || {
            let _p = tcp_recv(&c, 100)?;
            match tcp_wait_readable(&c) {
                Ok(_) => Ok(", waitReadable too?".into()),
                Err(e) => {
                    c.cancel_recv();
                    Ok(format!(", {}", error_text(&e)))
                }
            }
        });
        let w = tcp_wait_readable(&c)?;
        c.cancel_recv();
        drop(tcp_send(&sc, vec![args[1].clone().into_bytes()])?);
        sleep(50);
        println(&format!(
            "cancelled waitReadable resolved: {}",
            is_resolved(&w)
        ));
        drop(w);
        try_io("waitReadable", || {
            let p = tcp_wait_readable(&c)?;
            Ok(then(&p, |b| b.to_string()))
        });
        try_io("recv? 3", || {
            let p = tcp_recv(&c, 3)?;
            Ok(then(&p, bytes))
        });
        try_io("recv? 100", || {
            let p = tcp_recv(&c, 100)?;
            Ok(then(&p, bytes))
        });
        try_io("send of 4 buffers", || {
            let p = tcp_send(&c, vec![b"a".to_vec(), vec![], b"bc".to_vec(), vec![]])?;
            let r = then(&p, unit);
            let q = tcp_recv(&sc, 100)?;
            Ok(format!("{r}{}", then(&q, bytes)))
        });
        // shutdown behind a big write
        let big = vec![7u8; 32 * 1024 * 1024];
        let pb = tcp_send(&sc, vec![big])?;
        let sh = tcp_shutdown(&sc)?;
        try_io("shutdown while one is pending", || {
            tcp_shutdown(&sc).map(|_| String::new())
        });
        try_io("send while it is pending", || {
            tcp_send(&sc, vec![vec![1]]).map(|_| String::new())
        });
        let c2 = c.clone();
        let reader = as_task(
            move || -> R<u64> {
                let mut total = 0u64;
                while let Some(Ok(Some(b))) = wait(&tcp_recv(&c2, 65536)?) {
                    total += b.len() as u64;
                }
                Ok(total)
            },
            PRIO_DEDICATED,
        );
        println(&format!("big send{}", then(&pb, unit)));
        println(&format!("shutdown{}", then(&sh, unit)));
        println(&format!(
            "the client read {} bytes, then end of file",
            reader.get()?
        ));
        try_io("shutdown after it", || {
            tcp_shutdown(&sc).map(|_| String::new())
        });
        try_io("send after shutdown", || {
            tcp_send(&sc, vec![vec![1]]).map(|_| String::new())
        });
        try_io("waitReadable at end of file", || {
            let p = tcp_wait_readable(&c)?;
            Ok(then(&p, |b| b.to_string()))
        });
        try_io("recv? at end of file", || {
            let p = tcp_recv(&c, 100)?;
            Ok(then(&p, bytes))
        });
        try_io("recv? at end of file again", || {
            let p = tcp_recv(&c, 100)?;
            Ok(then(&p, bytes))
        });
        try_io("send to the half-closed peer", || {
            let p = tcp_send(&c, vec![b"late".to_vec()])?;
            Ok(then(&p, unit))
        });
        try_io("the peer reads it", || {
            let p = tcp_recv(&sc, 100)?;
            Ok(then(&p, bytes))
        });
        // a refused connect: a port that was bound, then closed
        let closed = {
            let t = new_tcp();
            t.bind(at(0))?;
            t.sock_name()?.port()
        };
        let d = new_tcp();
        try_io("connect to a closed port", || {
            let p = tcp_connect(&d, at(closed))?;
            Ok(then(&p, unit))
        });
        try_io("getPeerName after it", || {
            d.peer_name().map(|_| String::new())
        });
        try_io("getSockName after it", || {
            let a = d.sock_name()?;
            Ok(format!(", port chosen {}", a.port() != 0))
        });
        try_io("connect again after it", || {
            let p = tcp_connect(&d, at(closed))?;
            Ok(then(&p, unit))
        });
        drop(d);
        let s2 = new_tcp();
        try_io("bind to the port in use", || {
            s2.bind(at(port)).map(|_| String::new())
        });
        try_io("getSockName", || s2.sock_name().map(|_| String::new()));
        try_io("listen", || s2.listen(5).map(|_| String::new()));
        try_io("connect", || {
            let p = tcp_connect(&s2, at(port))?;
            Ok(then(&p, unit))
        });
        drop(s2);
        let s3 = new_tcp();
        s3.bind(at(0))?;
        try_io("connect an IPv4 socket to ::1", || {
            let p = tcp_connect(&s3, at6(std::net::Ipv6Addr::LOCALHOST, port))?;
            Ok(then(&p, unit))
        });
        drop(s3);
        println(&format!(
            "sockets kept: {} {} {}",
            s.sock_name()?.port() == port,
            sc.sock_name()?.port() == port,
            u.peer_name().is_err()
        ));
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// tcp_v6

fn shown(a: SocketAddr) -> String {
    match a {
        SocketAddr::V4(x) => format!("v4 {}", ip_text(&IpAddr::V4(*x.ip()))),
        SocketAddr::V6(x) => format!("v6 {}", ip_text(&IpAddr::V6(*x.ip()))),
    }
}

/// `get p`: the value, or the error thrown.
fn get<T: Clone + 'static>(p: P<T>) -> R<T> {
    match wait(&p) {
        Some(r) => r,
        None => Err(IoError::user_error("dropped")),
    }
}

fn tcp_v6(args: &[String]) -> u32 {
    run((|| {
        let ip = v6(&args[0]);
        let s = new_tcp();
        s.bind(at6(ip, 0))?;
        s.listen(4)?;
        let port = s.sock_name()?.port();
        println(&format!(
            "server: {}, port chosen {}",
            shown(s.sock_name()?),
            port != 0
        ));
        let c = new_tcp();
        get(tcp_connect(&c, at6(ip, port))?)?;
        println(&format!(
            "client peer: {}, port matches {}",
            shown(c.peer_name()?),
            c.peer_name()?.port() == port
        ));
        println(&format!("client name: {}", shown(c.sock_name()?)));
        let sc = get(tcp_accept(&s)?)?;
        println(&format!(
            "accepted peer: {}, is the client {}",
            shown(sc.peer_name()?),
            sc.peer_name()?.port() == c.sock_name()?.port()
        ));
        get(tcp_send(&c, vec![args[1].clone().into_bytes()])?)?;
        let m = get(tcp_recv(&sc, 100)?)?;
        println(&format!(
            "server got {}",
            m.as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("?")
        ));
        get(tcp_send(&sc, vec![b"back".to_vec()])?)?;
        get(tcp_shutdown(&sc)?)?;
        let r = get(tcp_recv(&c, 100)?)?;
        println(&format!(
            "client got {}",
            r.as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("?")
        ));
        let e = get(tcp_recv(&c, 100)?)?;
        println(&format!("then end of file: {}", e.is_none()));
        println(&format!(
            "server still there: {}",
            s.sock_name()?.port() == port
        ));
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// udp_basic (Std.Async.UDP)

fn a_usend(s: &UdpSocket, b: Vec<u8>, to: Option<SocketAddr>) -> Async<()> {
    let s = s.clone();
    Async::of_promise(move || udp_send(&s, vec![b], to))
}

fn a_urecv(s: &UdpSocket, n: u64) -> Async<Dgram> {
    let s = s.clone();
    Async::of_promise(move || udp_recv(&s, n))
}

fn udp_basic(args: &[String]) -> u32 {
    run((|| {
        let ip = v4(&args[0]);
        let at = |p| at4(ip, p);
        let a = new_udp();
        a.bind(at(0))?;
        let pa = a.sock_name()?.port();
        let b = new_udp();
        b.bind(at(0))?;
        let pb = b.sock_name()?.port();
        a_usend(&a, b"ping".to_vec(), Some(at(pb))).block()?;
        let (m, frm) = a_urecv(&b, 100).block()?;
        println(&format!(
            "b got {} from a: {}",
            utf8_opt(&m),
            frm.map(port) == Some(pa)
        ));
        let a2 = a.clone();
        let t = as_task(
            move || -> R<String> {
                let (m, _) = a_urecv(&a2, 100).block()?;
                Ok(utf8_opt(&m))
            },
            PRIO_DEDICATED,
        );
        sleep(20);
        a_usend(&b, b"pong".to_vec(), Some(at(pa))).block()?;
        println(&format!("a got {}", t.get()?));
        let c = new_udp();
        c.bind(at(0))?;
        c.connect(at(pb))?;
        println(&format!("peer: {}", c.peer_name()?.port() == pb));
        a_usend(&c, b"connected".to_vec(), None).block()?;
        let (m, _) = a_urecv(&b, 100).block()?;
        println(&format!("b got {}", utf8_opt(&m)));
        a_usend(&a, b"a long datagram".to_vec(), Some(at(pb))).block()?;
        let w = udp_wait_readable(&b)?;
        println(&format!("readable: {}", wait_bang(&w).is_ok()));
        drop(w);
        let (m, _) = a_urecv(&b, 6).block()?;
        println(&format!("truncated: {}", utf8_opt(&m)));
        b.set_broadcast(true)?;
        b.set_ttl(64)?;
        b.set_multicast_ttl(2)?;
        b.set_multicast_loop(false)?;
        println("options set");
        let d = new_udp();
        if let Err(e) = a_usend(&d, b"x".to_vec(), None).block() {
            println(&format!("send without address: {}", error_text(&e)));
        }
        if let Err(e) = a_usend(&c, b"x".to_vec(), Some(at(pa))).block() {
            println(&format!(
                "send with address when connected: {}",
                error_text(&e)
            ));
        }
        if let Err(e) = c.connect(at(pa)) {
            println(&format!("connect twice: {}", error_text(&e)));
        }
        if let Err(e) = b.set_ttl(0) {
            println(&format!("TTL 0: {}", error_text(&e)));
        }
        if let Err(e) = d.set_broadcast(true) {
            println(&format!("option without a descriptor: {}", error_text(&e)));
        }
        if let Err(e) = d.sock_name() {
            println(&format!("name without a descriptor: {}", error_text(&e)));
        }
        if let Err(e) = b.peer_name() {
            println(&format!(
                "peer of an unconnected socket: {}",
                error_text(&e)
            ));
        }
        println(&format!(
            "sockets kept: {} {}",
            a.sock_name()?.port() == pa,
            b.sock_name()?.port() == pb
        ));
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// udp_errors

fn dgram(r: Dgram) -> String {
    let from = match r.1 {
        Some(a) => ip_text(&a.ip()),
        None => "none".into(),
    };
    format!(
        "{} bytes {} from {from}",
        r.0.len(),
        std::str::from_utf8(&r.0).unwrap_or("?")
    )
}

fn udp_errors(args: &[String]) -> u32 {
    run((|| {
        let ip = v4(&args[0]);
        let ip6 = v6(&args[1]);
        let at = |p| at4(ip, p);
        let a = new_udp();
        a.bind(at(0))?;
        let pa = a.sock_name()?.port();
        let b = new_udp();
        b.bind(at(0))?;
        let pb = b.sock_name()?.port();
        let r = udp_recv(&b, 100)?;
        try_io("second recv", || udp_recv(&b, 100).map(|_| String::new()));
        try_io("waitReadable while receiving", || {
            udp_wait_readable(&b).map(|_| String::new())
        });
        b.cancel_recv();
        b.cancel_recv();
        println(&format!("cancelled recv resolved: {}", is_resolved(&r)));
        drop(r);
        try_io("empty array", || {
            let p = udp_send(&a, vec![], Some(at(pb)))?;
            Ok(then(&p, unit))
        });
        try_io("empty datagram", || {
            let p = udp_send(&a, vec![vec![]], Some(at(pb)))?;
            let s = then(&p, unit);
            let q = udp_recv(&b, 100)?;
            Ok(format!("{s}{}", then(&q, dgram)))
        });
        try_io("three buffers, one datagram", || {
            let p = udp_send(
                &a,
                vec![b"ab".to_vec(), vec![], b"cd".to_vec()],
                Some(at(pb)),
            )?;
            let s = then(&p, unit);
            let q = udp_recv(&b, 100)?;
            Ok(format!("{s}{}", then(&q, dgram)))
        });
        try_io("a datagram over 65507 bytes", || {
            let p = udp_send(&a, vec![vec![1u8; 70000]], Some(at(pb)))?;
            Ok(then(&p, unit))
        });
        let closed = {
            let t = new_udp();
            t.bind(at(0))?;
            t.sock_name()?.port()
        };
        let c = new_udp();
        c.connect(at(closed))?;
        try_io("send to a closed port", || {
            let p = udp_send(&c, vec![b"x".to_vec()], None)?;
            Ok(then(&p, unit))
        });
        try_io("recv after it", || {
            let q = udp_recv(&c, 100)?;
            Ok(then(&q, dgram))
        });
        try_io("TTL 255", || b.set_ttl(255).map(|_| String::new()));
        try_io("TTL 256", || b.set_ttl(256).map(|_| String::new()));
        try_io("TTL 2^31", || b.set_ttl(0x8000_0000).map(|_| String::new()));
        try_io("multicast TTL 0", || {
            b.set_multicast_ttl(0).map(|_| String::new())
        });
        try_io("multicast TTL 256", || {
            b.set_multicast_ttl(256).map(|_| String::new())
        });
        try_io("multicast loop", || {
            b.set_multicast_loop(true).map(|_| String::new())
        });
        try_io("multicast interface", || {
            b.set_multicast_interface(IpAddr::V4(ip))
                .map(|_| String::new())
        });
        try_io("multicast interface IPv6 on an IPv4 socket", || {
            b.set_multicast_interface(IpAddr::V6(ip6))
                .map(|_| String::new())
        });
        let g = IpAddr::V4(std::net::Ipv4Addr::new(239, 1, 2, 3));
        let g6 = IpAddr::V6(std::net::Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1));
        try_io("membership 2", || {
            b.set_membership(g, None, 2).map(|_| String::new())
        });
        try_io("leave a group never joined", || {
            b.set_membership(g, None, 0).map(|_| String::new())
        });
        try_io("leave with an IPv6 interface", || {
            b.set_membership(g, Some(IpAddr::V6(ip6)), 0)
                .map(|_| String::new())
        });
        try_io("IPv6 group on an IPv4 socket", || {
            b.set_membership(g6, None, 0).map(|_| String::new())
        });
        let v = new_udp();
        v.bind(at6(ip6, 0))?;
        try_io("IPv6 TTL", || v.set_ttl(3).map(|_| String::new()));
        try_io("IPv6 multicast TTL", || {
            v.set_multicast_ttl(3).map(|_| String::new())
        });
        try_io("IPv6 multicast loop", || {
            v.set_multicast_loop(false).map(|_| String::new())
        });
        try_io("IPv6 multicast interface", || {
            v.set_multicast_interface(IpAddr::V6(ip6))
                .map(|_| String::new())
        });
        try_io("IPv6 leave a group never joined", || {
            v.set_membership(g6, None, 0).map(|_| String::new())
        });
        try_io("IPv6 group with an IPv4 interface", || {
            v.set_membership(g6, Some(IpAddr::V4(ip)), 0)
                .map(|_| String::new())
        });
        try_io("IPv6 datagram to itself", || {
            let p = udp_send(&v, vec![b"six".to_vec()], Some(v.sock_name()?))?;
            let s = then(&p, unit);
            let q = udp_recv(&v, 100)?;
            Ok(format!("{s}{}", then(&q, dgram)))
        });
        println(&format!(
            "sockets kept: {} {}",
            a.sock_name()?.port() == pa,
            b.sock_name()?.port() == pb
        ));
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// dns_localhost, dns_pending_at_exit

fn ips(a: &[IpAddr]) -> String {
    let v: Vec<String> = a.iter().map(ip_text).collect();
    format!("#[{}]", v.join(", "))
}

fn gai(h: &str, s: &str, f: u8) {
    let head = format!("{} {} {f}", quote(h), quote(s));
    match get_addr_info(h, s, f) {
        Err(e) => println(&format!("{head}: {}", error_text(&e))),
        Ok(p) => match wait(&p) {
            None => println(&format!("{head}: dropped")),
            Some(Ok(a)) => println(&format!("{head}: {}", ips(&a))),
            Some(Err(e)) => println(&format!("{head}: then {}", error_text(&e))),
        },
    }
}

fn gni(a: SocketAddr) {
    let shown = format!("{} {}", ip_text(&a.ip()), a.port());
    match get_name_info(a) {
        Err(e) => println(&format!("name of {shown}: {}", error_text(&e))),
        Ok(p) => match wait(&p) {
            None => println(&format!("name of {shown}: dropped")),
            Some(Ok((h, s))) => println(&format!("name of {shown}: {} {}", quote(&h), quote(&s))),
            Some(Err(e)) => println(&format!("name of {shown}: then {}", error_text(&e))),
        },
    }
}

fn dns_localhost(args: &[String]) -> u32 {
    let host = &args[0];
    let v4s = &args[1];
    let v6s = &args[2];
    let hosts = [
        host.clone(),
        host.to_uppercase(),
        format!("{host}."),
        v4s.clone(),
        v6s.clone(),
        "1.2.3".into(),
    ];
    for h in &hosts {
        for s in ["", "0", "8080"] {
            for f in [0u8, 1, 2, 3] {
                gai(h, s, f);
            }
        }
    }
    gai(v4s, "no-such-service-here", 0);
    gai("", "", 0);
    gai("a b", "", 0);
    gai(host, "é", 0);
    gai(&"a".repeat(256), "", 0);
    for p in [0u16, 65535, 49999] {
        gni(at4(v4(v4s), p));
        gni(at6(v6(v6s), p));
    }
    0
}

fn dns_pending_at_exit(args: &[String]) -> u32 {
    run((|| {
        let p = get_addr_info(&args[0], "", 0)?;
        println("started");
        drop(p);
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// iface_lo

fn iface_lo(args: &[String]) -> u32 {
    run((|| {
        let all = iface::interface_addresses()?;
        let mine: Vec<_> = all.iter().filter(|i| i.name == args[0]).collect();
        println(&format!("{} entries for {}", mine.len(), args[0]));
        for i in mine {
            let hw: Vec<String> = i.physical_address.iter().map(|b| b.to_string()).collect();
            println(&format!(
                "{} loopback {} {} mask {} hw #[{}]",
                i.name,
                i.is_loopback,
                ip_text(&i.address),
                ip_text(&i.netmask),
                hw.join(", ")
            ));
        }
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// accept_parallel, accept_parallel_try (the judge's A_Accept)

fn accept_parallel(args: &[String]) -> u32 {
    run((|| {
        let mode = args.first().map(String::as_str).unwrap_or("accept");
        let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
        let s = new_tcp();
        s.bind(lo(0))?;
        s.listen(16)?;
        let port = s.sock_name()?.port();
        let p1 = tcp_accept(&s)?;
        match mode {
            "accept" => match tcp_accept(&s) {
                Ok(_) => println("second accept: ok"),
                Err(e) => println(&format!("second accept: {}", error_text(&e))),
            },
            "tryAccept" => match s.try_accept() {
                Ok(_) => println("tryAccept: ok"),
                Err(e) => println(&format!("tryAccept: {}", error_text(&e))),
            },
            _ => {}
        }
        let c = new_tcp();
        let pc = tcp_connect(&c, lo(port))?;
        println("connect started");
        let _ = lean_runtime::io::Handle::stdout().flush();
        match wait(&pc) {
            Some(Ok(())) => println("connect: ok"),
            Some(Err(e)) => println(&format!("connect: {}", error_text(&e))),
            None => println("connect: dropped"),
        }
        match wait(&p1) {
            Some(Ok(_)) => println("first accept: ok"),
            Some(Err(e)) => println(&format!("first accept: {}", error_text(&e))),
            None => println("first accept: dropped"),
        }
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// keepalive_zero_delay (the judge's C_KeepAlive)

fn attempt(what: &str, r: R<()>) {
    match r {
        Ok(()) => println(&format!("{what}: ok")),
        Err(e) => println(&format!("{what}: {}", error_text(&e))),
    }
}

fn keepalive_zero_delay(_: &[String]) -> u32 {
    run((|| {
        let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
        let fresh = new_tcp();
        attempt("fresh (no fd) keepAlive 1 0", fresh.keep_alive(1, 0));
        attempt("fresh bind after keepAlive 1 0", fresh.bind(lo(0)));
        drop(fresh);
        let s = new_tcp();
        s.bind(lo(0))?;
        attempt("bound keepAlive 1 0", s.keep_alive(1, 0));
        attempt("bound keepAlive 1 0 again", s.keep_alive(1, 0));
        attempt("bound keepAlive 0 0", s.keep_alive(0, 0));
        attempt("bound keepAlive 1 1", s.keep_alive(1, 1));
        attempt(
            "bound keepAlive 1 40000 (above Linux's TCP_KEEPIDLE max)",
            s.keep_alive(1, 40000),
        );
        s.listen(4)?;
        let port = s.sock_name()?.port();
        let c = new_tcp();
        let _ = wait(&tcp_connect(&c, lo(port))?);
        attempt("connected keepAlive 1 0", c.keep_alive(1, 0));
        attempt("connected keepAlive 1 30", c.keep_alive(1, 30));
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// multicast_ipv6_long (the judge's B_Multicast)

fn mcast_attempt(what: &str, r: R<()>) {
    attempt(what, r);
    // `(← IO.getStdout).flush`
    let _ = lean_runtime::io::Handle::stdout().flush();
}

fn multicast_ipv6_long(args: &[String]) -> u32 {
    run((|| {
        let u = new_udp();
        u.bind(at6(std::net::Ipv6Addr::LOCALHOST, 0))?;
        let g =
            |a, b, c, d, e, f, g, h| IpAddr::V6(std::net::Ipv6Addr::new(a, b, c, d, e, f, g, h));
        let g7 = g(0xff02, 0, 0, 0, 0, 0, 0, 1);
        let g15 = g(0xff02, 0, 0, 0, 0, 1, 0xff00, 0x12);
        let g16 = g(0xff02, 0, 0, 0, 0, 1, 0xff00, 0x123);
        let g17 = g(0xff02, 0, 0, 0, 0, 1, 0xff00, 0x1234);
        let i15 = g(0xfe80, 0, 0, 0, 0, 0, 0x1234, 0x5678);
        let i20 = g(0xfe80, 0, 0, 0, 0, 0x1234, 0x5678, 0x9abc);
        let group = |a: IpAddr| {
            let t = ip_text(&a);
            println(&format!("text {t} ({})", t.chars().count()));
            mcast_attempt("join", u.set_membership(a, None, 1));
            mcast_attempt("leave", u.set_membership(a, None, 0));
        };
        let interface = |a: IpAddr| {
            let t = ip_text(&a);
            println(&format!("text {t} ({})", t.chars().count()));
            mcast_attempt("multicast interface", u.set_multicast_interface(a));
        };
        for m in args {
            match m.as_str() {
                "g7" => group(g7),
                "g15" => group(g15),
                "g16" => group(g16),
                "g17" => group(g17),
                "i15" => interface(i15),
                "i20" => interface(i20),
                "g7i20" => mcast_attempt(
                    "join g7 on interface i20",
                    u.set_membership(g7, Some(i20), 1),
                ),
                "einval" => mcast_attempt("membership 2", u.set_membership(g7, None, 2)),
                _ => println(&format!("unknown {m}")),
            }
        }
        println("after");
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// recv_huge_* (the judge's D_RecvHuge)

fn recv_huge(args: &[String]) -> u32 {
    let mode = args.first().map(String::as_str).unwrap_or("tcp-max");
    println(&format!("mode {mode}"));
    let _ = lean_runtime::io::Handle::stdout().flush();
    let report = |r: R<()>| match r {
        Ok(()) => println("returned"),
        Err(e) => println(&format!("error: {}", error_text(&e))),
    };
    match mode {
        "tcp-max" => report(tcp_recv(&new_tcp(), u64::MAX).map(|_| ())),
        "tcp-25" => report(tcp_recv(&new_tcp(), u64::MAX - 24).map(|_| ())),
        "udp-max" => report(udp_recv(&new_udp(), u64::MAX).map(|_| ())),
        _ => println("unknown"),
    }
    println("after");
    0
}

// ---------------------------------------------------------------------------
// The leak cases (upstream's shape)

/// `(← System.FilePath.readDir "/dev/fd").size`.
fn open_fds() -> R<usize> {
    let mut n = 0;
    lean_runtime::io::fs::read_dir(b"/dev/fd", |_| n += 1)?;
    Ok(n)
}

fn udp_cancel_recv_leak(args: &[String]) -> u32 {
    run((|| {
        let ip = v4(&args[0]);
        let before = open_fds()?;
        for _ in 0..64 {
            let u = new_udp();
            u.bind(at4(ip, 0))?;
            drop(udp_recv(&u, 64)?);
            u.cancel_recv();
        }
        let n = open_fds()?.saturating_sub(before);
        println(&if n <= 16 {
            "ok".to_string()
        } else {
            format!("leaked {n} file descriptors")
        });
        Ok(())
    })())
}

fn tcp_shutdown_fail_leak(args: &[String]) -> u32 {
    run((|| {
        let ip = v4(&args[0]);
        let first = new_tcp();
        let before = open_fds()?;
        for _ in 0..64 {
            let t = new_tcp();
            t.bind(at4(ip, 0))?;
            let _ = tcp_shutdown(&t);
        }
        let n = open_fds()?.saturating_sub(before);
        println(&if n <= 16 {
            "ok".to_string()
        } else {
            format!("leaked {n} file descriptors")
        });
        let _ = first.sock_name();
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// recv_zero_* and udp_recv_zero (the judge's R_Recv0)

fn show_recv(r: Option<R<Option<Vec<u8>>>>) -> String {
    match r {
        None => "dropped".into(),
        Some(Ok(None)) => "ok none".into(),
        Some(Ok(Some(b))) => match std::str::from_utf8(&b) {
            Ok(s) => format!("ok some some {}", quote(s)),
            Err(_) => "ok some none".into(),
        },
        Some(Err(e)) => format!("error {}", error_text(&e)),
    }
}

/// `pair`: a listening socket, a client and the accepted peer.
fn pair() -> R<(TcpSocket, TcpSocket, TcpSocket)> {
    let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
    let s = new_tcp();
    s.bind(lo(0))?;
    s.listen(16)?;
    let port = s.sock_name()?.port();
    let c = new_tcp();
    let pc = tcp_connect(&c, lo(port))?;
    let pa = tcp_accept(&s)?;
    let _ = wait(&pc);
    match wait(&pa) {
        Some(Ok(peer)) => Ok((s, c, peer)),
        _ => Err(IoError::user_error("accept failed")),
    }
}

fn recv_n(c: &TcpSocket, n: u64) -> R<()> {
    let r = wait(&tcp_recv(c, n)?);
    println(&format!("recv? {n}: {}", show_recv(r)));
    Ok(())
}

fn recv_zero(args: &[String]) -> u32 {
    run((|| {
        match args.first().map(String::as_str).unwrap_or("data") {
            "data" => {
                let (_s, c, peer) = pair()?;
                let _ = wait(&tcp_send(&peer, vec![b"hello".to_vec()])?);
                recv_n(&c, 0)?;
                recv_n(&c, 0)?;
                recv_n(&c, 16)?;
            }
            "eof" => {
                let (_s, c, peer) = pair()?;
                let _ = wait(&tcp_shutdown(&peer)?);
                recv_n(&c, 0)?;
                recv_n(&c, 16)?;
                recv_n(&c, 0)?;
            }
            "dataeof" => {
                let (_s, c, peer) = pair()?;
                let _ = wait(&tcp_send(&peer, vec![b"hello".to_vec()])?);
                let _ = wait(&tcp_shutdown(&peer)?);
                recv_n(&c, 0)?;
                recv_n(&c, 16)?;
                recv_n(&c, 0)?;
                recv_n(&c, 16)?;
            }
            "udp" => {
                let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
                let u = new_udp();
                u.bind(lo(0))?;
                let v = new_udp();
                v.bind(lo(0))?;
                let _ = wait(&udp_send(&v, vec![b"hi".to_vec()], Some(u.sock_name()?))?);
                for n in [0u64, 16] {
                    match wait(&udp_recv(&u, n)?) {
                        Some(Ok((b, a))) => println(&format!(
                            "recv {n}: ok {} from sender {}",
                            match std::str::from_utf8(&b) {
                                Ok(s) => format!("some {}", quote(s)),
                                Err(_) => "none".into(),
                            },
                            a.is_some()
                        )),
                        Some(Err(e)) => println(&format!("recv {n}: error {}", error_text(&e))),
                        None => println(&format!("recv {n}: dropped")),
                    }
                }
            }
            m => println(&format!("unknown {m}")),
        }
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// shutdown_* (the judge's RNET02_Probe; LB-28)

/// `say s`: a line, then a flush of stdout.
fn say(s: &str) {
    println(s);
    let _ = lean_runtime::io::Handle::stdout().flush();
}

/// `waitFor p fmt`: the promise's value if it resolves within 2 s (40 waits
/// of 50 ms), else `pending`.
fn wait_for<T: Clone + 'static>(p: &P<T>, fmt: impl FnOnce(Option<R<T>>) -> String) -> String {
    for _ in 0..40 {
        if is_resolved(p) {
            return fmt(wait(p));
        }
        sleep(50);
    }
    "pending".into()
}

fn unit_res(r: Option<R<()>>) -> String {
    match r {
        None => "dropped".into(),
        Some(Ok(())) => "ok".into(),
        Some(Err(e)) => format!("error {}", error_text(&e)),
    }
}

fn recv_res(r: Option<R<Option<Vec<u8>>>>) -> String {
    match r {
        None => "dropped".into(),
        Some(Ok(None)) => "ok none (end of stream)".into(),
        Some(Ok(Some(b))) => match std::str::from_utf8(&b) {
            Ok(s) => format!("ok some some {}", quote(s)),
            Err(_) => "ok some none".into(),
        },
        Some(Err(e)) => format!("error {}", error_text(&e)),
    }
}

fn shutdown_connect(args: &[String]) -> u32 {
    run((|| {
        let mode = args.first().map(String::as_str).unwrap_or("early");
        let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
        let s = new_tcp();
        s.bind(lo(0))?;
        s.listen(16)?;
        let port = s.sock_name()?.port();
        let pa = tcp_accept(&s)?;
        let c = new_tcp();
        let pc = tcp_connect(&c, lo(port))?;
        let ps = match mode {
            "after" => {
                say(&format!("connect: {}", wait_for(&pc, unit_res)));
                tcp_shutdown(&c)?
            }
            "write" => {
                drop(tcp_send(&c, vec![b"x".to_vec()])?);
                tcp_shutdown(&c)?
            }
            _ => tcp_shutdown(&c)?,
        };
        if mode != "after" {
            say(&format!("connect: {}", wait_for(&pc, unit_res)));
        }
        drop(pc);
        let peer = match wait(&pa) {
            Some(Ok(p)) => p,
            _ => return Err(IoError::user_error("accept failed")),
        };
        drop(pa);
        // the program's last use of `c` is above
        drop(c);
        say(&format!("shutdown: {}", wait_for(&ps, unit_res)));
        say(&format!(
            "peer recv?: {}",
            wait_for(&tcp_recv(&peer, 64)?, recv_res)
        ));
        if mode == "write" {
            say(&format!(
                "peer recv?: {}",
                wait_for(&tcp_recv(&peer, 64)?, recv_res)
            ));
        }
        say("done");
        Ok(())
    })())
}

// ---------------------------------------------------------------------------
// Regression programs of net-1's review (not cases)

/// RNET-01: the translator's `alloc` of a receive ends the process through
/// Lean's internal panic, whose effect point lets a task that uses the same
/// socket run (here every millisecond). The socket is not borrowed across
/// `alloc`, so the program ends as native does: `INTERNAL PANIC: integer
/// overflow in runtime computation`, exit 1 (not Rust's "already borrowed",
/// exit 101). Argument `tcp` or `udp`.
fn rnet_alloc_reentry(args: &[String]) -> u32 {
    let udp = args.first().map(String::as_str) == Some("udp");
    let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
    let (_s, c, _peer) = pair().expect("a connected pair");
    let u = new_udp();
    u.bind(lo(0)).expect("bind");
    let (c2, u2) = (c.clone(), u.clone());
    let _t = as_task(
        move || loop {
            sleep(1);
            if udp {
                let _ = u2.set_ttl(5);
            } else {
                let _ = c2.no_delay();
            }
        },
        PRIO_DEDICATED,
    );
    sleep(20);
    println("calling recv with a huge size");
    let _ = lean_runtime::io::Handle::stdout().flush();
    // the task's next sleep ends while `main` computes, without a yield
    std::thread::sleep(std::time::Duration::from_millis(30));
    if udp {
        let _ = udp_recv(&u, u64::MAX);
    } else {
        let _ = tcp_recv(&c, u64::MAX);
    }
    println("returned");
    0
}

/// RNET-02 (LB-28), made deterministic: `connect` and `shutdown` in a
/// no-suspend scope, where the externs do not let the loop run first, so
/// the shutdown is requested while the connect is surely pending (natively
/// the loop thread almost never gets in between). The shutdown happens once
/// the connect succeeds: `shutdown: ok`, and the peer reads end of stream.
/// A connect that fails takes the queued shutdown with it (`ECANCELED`).
fn rnet_shutdown_in_connect(_: &[String]) -> u32 {
    run((|| {
        let lo = |p| at4(std::net::Ipv4Addr::LOCALHOST, p);
        let s = new_tcp();
        s.bind(lo(0))?;
        s.listen(16)?;
        let port = s.sock_name()?.port();
        let pa = tcp_accept(&s)?;
        let c = new_tcp();
        let (pc, ps) = {
            let _g = lean_runtime::sched::no_suspend();
            let pc = tcp_connect(&c, lo(port))?;
            let ps = tcp_shutdown(&c)?;
            (pc, ps)
        };
        drop(c);
        println(&format!("connect: {}", unit_res(wait(&pc))));
        println(&format!("shutdown: {}", unit_res(wait(&ps))));
        let peer = match wait(&pa) {
            Some(Ok(p)) => p,
            _ => return Err(IoError::user_error("accept failed")),
        };
        println(&format!(
            "peer recv?: {}",
            recv_res(wait(&tcp_recv(&peer, 64)?))
        ));
        // a refused connect with a shutdown behind it
        let closed = {
            let t = new_tcp();
            t.bind(lo(0))?;
            t.sock_name()?.port()
        };
        let d = new_tcp();
        let (pc, ps) = {
            let _g = lean_runtime::sched::no_suspend();
            let pc = tcp_connect(&d, lo(closed))?;
            let ps = tcp_shutdown(&d)?;
            (pc, ps)
        };
        drop(d);
        println(&format!("refused connect: {}", unit_res(wait(&pc))));
        println(&format!("its shutdown: {}", unit_res(wait(&ps))));
        Ok(())
    })())
}
