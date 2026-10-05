//! Unit tests of `net`'s pure parts, of the calls that need no event loop,
//! and of a socket's lifetime on the single-thread scheduler's loop
//! (AR-12); `tests_mt.rs` has threads mode's (docs/threads.md, 0.7). The
//! program cases (`tests/cases/net`, through `tests/sched-driver` and
//! `tests/sched-driver-mt`) cover the rest with native Lean's outcomes.

use super::dns::{
    answer, idna_ok, safe_ascii, translate_eai, Answer, Fail, Job, Pool, Query, Raw, EAI_FAIL,
    EAI_OVERFLOW, EAI_SYSTEM,
};
use super::iface::{from_entries, Entry};
use super::tcp::advance;
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
use super::tcp::TcpSocket;
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
use super::udp::UdpSocket;
use super::*;
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
use crate::sched;
use rustix::fd::AsFd;
use std::net::{Ipv4Addr, Ipv6Addr};
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
use std::rc::Rc;

/// libuv's `uv__getaddrinfo_translate_error` over glibc's codes, and how
/// Lean shows the results (`otherError`, libuv's message).
#[test]
fn eai_codes() {
    let table = [
        (-9, -3000, "address family not supported"),
        (-3, -3001, "temporary failure"),
        (-1, -3002, "bad ai_flags value"),
        (-101, -3003, "request canceled"),
        (-4, -3004, "permanent failure"),
        (-6, -3005, "ai_family not supported"),
        (-10, -3006, "out of memory"),
        (-5, -3007, "no address"),
        (-2, -3008, "unknown node or service"),
        (-12, -3009, "argument buffer overflow"),
        (-8, -3010, "service not available for socket type"),
        (-7, -3011, "socket type not supported"),
    ];
    for (eai, uv, msg) in table {
        assert_eq!(translate_eai(eai, 0), uv, "EAI {eai}");
        assert_eq!(uv_error(uv), IoError::OtherError(-uv as u32, msg.into()));
    }
    // `EAI_SYSTEM`: the `errno`
    assert_eq!(translate_eai(-11, 24), -24);
    assert_eq!(translate_eai(0, 0), 0);
}

/// `is_safe_ascii_str` and libuv's IDNA limits (native outcomes in case
/// `dns_localhost`).
#[test]
fn dns_checks() {
    assert!(safe_ascii(b"localhost"));
    assert!(safe_ascii(b"a-b_c.d:e/f+g~h@i=j,k%l09AZ"));
    assert!(safe_ascii(b""));
    for bad in [
        &b"a b"[..],
        b"\xc3\xa9",
        b"a\0b",
        b"a*",
        b"a#b",
        b"a!",
        b"[::1]",
    ] {
        assert!(!safe_ascii(bad), "{bad:?}");
    }
    assert!(!idna_ok(b""));
    assert!(idna_ok(&[b'a'; 255]));
    assert!(!idna_ok(&[b'a'; 256]));
}

/// The checks that fail before a lookup starts, with Lean's errors.
#[test]
fn dns_sync_errors() {
    // threads mode: it takes the loop lock, which `tests_mt`'s tests count
    #[cfg(all(feature = "threads", not(feature = "sched")))]
    let _s = crate::sched::mt::test_serial();
    let not_ascii = |m: &str| IoError::InvalidArgument(None, 22, m.into());
    let r = dns::get_addr_info("a b", "", 0, |_| unreachable!());
    assert_eq!(r, Err(not_ascii("name is not ASCII")));
    let r = dns::get_addr_info("localhost", "é", 0, |_| unreachable!());
    assert_eq!(r, Err(not_ascii("service is not ASCII")));
    let r = dns::get_addr_info("", "", 0, |_| unreachable!());
    assert_eq!(r, Err(uv_error(UV_EINVAL)));
    let long = "a".repeat(256);
    let r = dns::get_addr_info(&long, "", 0, |_| unreachable!());
    assert_eq!(r, Err(uv_error(UV_EINVAL)));
}

/// `uv__write_req_update`: the position after a write, empty buffers passed
/// one per call.
#[test]
fn write_progress() {
    let bufs: [&[u8]; 4] = [b"a", b"", b"bc", b""];
    let len = |i: usize| bufs[i].len();
    // writev of all four gives 3 bytes: the trailing empty buffer is left
    assert_eq!(advance(&len, 4, 0, 0, 3), (3, 0));
    // then a write of 0 bytes passes it
    assert_eq!(advance(&len, 4, 3, 0, 0), (4, 0));
    // a partial write stops inside a buffer
    assert_eq!(advance(&len, 4, 2, 0, 1), (2, 1));
    assert_eq!(advance(&len, 4, 2, 1, 1), (3, 0));
    // one byte: stops at the empty buffer after it, without passing it
    assert_eq!(advance(&len, 4, 0, 0, 1), (1, 0));
    // two empty buffers take two writes of 0 bytes
    let e: [&[u8]; 2] = [b"", b""];
    let elen = |i: usize| e[i].len();
    assert_eq!(advance(&elen, 2, 0, 0, 0), (1, 0));
    assert_eq!(advance(&elen, 2, 1, 0, 0), (2, 0));
}

/// What a watch callback hands libuv's stream and UDP code.
#[test]
fn events() {
    let r = Ready {
        read: true,
        write: false,
        hangup: false,
        error: false,
    };
    let e = Ev::from_ready(r, Interest::READ);
    assert!(e.read && !e.write && !e.hup);
    // an error or a hang-up reaches both directions of a stream
    let r = Ready {
        read: true,
        write: true,
        hangup: true,
        error: true,
    };
    let e = Ev::from_ready(r, Interest::READ);
    assert!(e.read && e.write && e.hup);
    // a UDP handle: libuv's merge (raw IN, raw OUT, ERR, HUP)
    let both = Interest::BOTH;
    let e = Ev::libuv_merge(true, false, false, false, both);
    assert!(e.read && !e.write);
    // what is not watched is dropped
    let e = Ev::libuv_merge(true, true, false, false, Interest::WRITE);
    assert!(!e.read && e.write);
    // an error with POLLOUT: send, do not receive
    let e = Ev::libuv_merge(false, true, true, false, both);
    assert!(!e.read && e.write);
    // exactly POLLERR, or exactly POLLHUP: the watched events are added
    let e = Ev::libuv_merge(false, false, true, false, Interest::READ);
    assert!(e.read && !e.write);
    let e = Ev::libuv_merge(false, false, false, true, both);
    assert!(e.read && e.write && e.hup);
    // POLLERR and POLLHUP together: neither
    let e = Ev::libuv_merge(false, false, true, true, both);
    assert!(!e.read && !e.write);
    const { assert!(Ev::FEED.write && !Ev::FEED.read) };
}

/// A Lean `SocketAddress` as a system address: no flow label, no scope.
#[test]
fn addresses() {
    let a = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 7, 9, 3));
    let back = socket_address(sockaddr(&a)).unwrap();
    assert_eq!(
        back,
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 7, 0, 0))
    );
    let a = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 1, 2, 3), 65535));
    assert_eq!(socket_address(sockaddr(&a)), Some(a));
}

fn entry(
    name: &str,
    up: bool,
    ip: Option<IpAddr>,
    mask: Option<IpAddr>,
    link: Option<[u8; 6]>,
) -> Entry {
    Entry {
        name: name.into(),
        up_running: up,
        loopback: name == "lo",
        ip,
        link,
        has_addr: ip.is_some() || link.is_some(),
        netmask: mask,
    }
}

/// `uv_interface_addresses` and Lean's conversion: addresses of interfaces
/// up and running, in order; hardware addresses from link-layer entries,
/// aliases included.
#[test]
fn interfaces() {
    let v4 = |a, b, c, d| Some(IpAddr::V4(Ipv4Addr::new(a, b, c, d)));
    let mac = [1, 2, 3, 4, 5, 6];
    let es = [
        entry("lo", true, None, None, Some([0; 6])),
        entry("eth0", true, None, None, Some(mac)),
        entry("down0", false, None, None, Some([9; 6])),
        entry("lo", true, v4(127, 0, 0, 1), v4(255, 0, 0, 0), None),
        entry("eth0", true, v4(10, 0, 0, 2), v4(255, 255, 255, 0), None),
        entry("eth0:1", true, v4(10, 0, 0, 3), v4(255, 255, 255, 0), None),
        entry("eth01", true, v4(10, 0, 1, 3), None, None),
        entry("down0", false, v4(10, 9, 9, 9), None, None),
        entry("nothing", true, None, None, None),
        entry(
            "lo",
            true,
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            Some(IpAddr::V6(Ipv6Addr::from(u128::MAX))),
            None,
        ),
    ];
    let got = from_entries(&es);
    let names: Vec<&str> = got.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["lo", "eth0", "eth0:1", "eth01", "lo"]);
    assert_eq!(got[0].physical_address, [0; 6]);
    assert!(got[0].is_loopback);
    assert_eq!(got[1].physical_address, mac);
    // an alias takes its interface's hardware address
    assert_eq!(got[2].physical_address, mac);
    // `eth01` is not an alias of `eth0`
    assert_eq!(got[3].physical_address, [0; 6]);
    // no netmask: zeros of the address's family
    assert_eq!(got[3].netmask, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(got[4].netmask, IpAddr::V6(Ipv6Addr::from(u128::MAX)));
}

/// `recv_into` reads once, at most the size asked for, into each kind of
/// target.
#[test]
fn recv_into_targets() {
    use std::io::Write;
    use std::mem::MaybeUninit;
    use std::os::unix::net::UnixStream;
    let (a, mut b) = UnixStream::pair().unwrap();
    b.write_all(b"hello world").unwrap();
    // a Vec with exactly the room asked for
    let mut v = Vec::with_capacity(5);
    let n = recv_into(a.as_fd(), RecvTarget::Vec(&mut v, 5), false).unwrap();
    assert_eq!((n.0, v.as_slice()), (5, &b"hello"[..]));
    // a Vec with more room: no more than asked for
    let mut v = Vec::with_capacity(64);
    v.extend_from_slice(b"x");
    let n = recv_into(a.as_fd(), RecvTarget::Vec(&mut v, 3), false).unwrap();
    assert_eq!((n.0, v.as_slice()), (3, &b"x wo"[..]));
    // uninitialized storage
    let mut u = [MaybeUninit::<u8>::uninit(); 16];
    let n = recv_into(a.as_fd(), RecvTarget::Uninit(&mut u), false).unwrap();
    assert_eq!(n.0, 3);
    // nothing left: the socket would block (`EAGAIN` once non-blocking)
    rustix::fs::fcntl_setfl(&a, rustix::fs::OFlags::NONBLOCK).unwrap();
    let mut v = Vec::with_capacity(4);
    let r = recv_into(a.as_fd(), RecvTarget::Vec(&mut v, 4), false);
    assert_eq!(r.map(|x| x.0), Err(rustix::io::Errno::AGAIN));
    assert!(v.is_empty());
}

/// Calls on sockets that need no event loop: libuv's errors before a
/// descriptor exists, and after `bind` (native outcomes in cases
/// `tcp_errors`, `keepalive_zero_delay` and `udp_basic`). In threads mode
/// each takes the loop lock (the loop thread is made at the first).
#[test]
fn sockets_without_a_loop() {
    use super::tcp::TcpSocket;
    use super::udp::UdpSocket;
    #[cfg(all(feature = "threads", not(feature = "sched")))]
    let _s = crate::sched::mt::test_serial();
    let lo = |p| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p);
    let ebadf = IoError::InvalidArgument(None, 9, "bad file descriptor".into());
    let enotconn = IoError::InvalidArgument(None, 107, "socket is not connected".into());
    let t = TcpSocket::new().unwrap();
    assert_eq!(t.peer_name(), Err(ebadf.clone()));
    assert_eq!(t.sock_name(), Err(ebadf.clone()));
    assert_eq!(t.no_delay(), Ok(()));
    // no descriptor yet: the setting waits for it, whatever the delay
    assert_eq!(t.keep_alive(1, 0), Ok(()));
    t.bind(lo(0)).unwrap();
    assert!(t.sock_name().unwrap().port() != 0);
    assert_eq!(t.peer_name(), Err(enotconn.clone()));
    // LB-23: `EINVAL`, where libuv 1.48 says `EPERM`
    assert_eq!(t.keep_alive(1, 0), Err(uv_error(UV_EINVAL)));
    assert_eq!(t.keep_alive(0, 0), Ok(()));
    assert_eq!(t.keep_alive(1, 40000), Err(uv_error(UV_EINVAL)));
    assert_eq!(t.bind(lo(0)), Err(uv_error(UV_EINVAL)));
    // a bind to a listening socket's port: reported later
    let occupant = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupant.local_addr().unwrap().port();
    let t2 = TcpSocket::new().unwrap();
    assert_eq!(t2.bind(lo(port)), Ok(()));
    let busy = IoError::ResourceBusy(98, "address already in use".into());
    assert_eq!(t2.sock_name(), Err(busy.clone()));
    assert_eq!(t2.listen(4), Err(busy));
    let u = UdpSocket::new().unwrap();
    assert_eq!(u.sock_name(), Err(ebadf.clone()));
    assert_eq!(u.set_broadcast(true), Err(ebadf.clone()));
    assert_eq!(u.set_ttl(0), Err(uv_error(UV_EINVAL)));
    assert_eq!(u.set_ttl(64), Err(ebadf));
    assert_eq!(u.set_multicast_ttl(256), Err(uv_error(UV_EINVAL)));
    u.bind(lo(0)).unwrap();
    assert_eq!(u.peer_name(), Err(enotconn));
    assert_eq!(u.set_ttl(64), Ok(()));
    assert_eq!(
        u.set_membership(IpAddr::V4(Ipv4Addr::new(239, 1, 2, 3)), None, 2),
        Err(uv_error(UV_EINVAL))
    );
}

/// libuv's reading of glibc's answers: `EAI_SYSTEM` with `errno` 0 is a
/// success with no entries for `getAddrInfo` (native: `ok #[]`).
#[test]
fn answers() {
    let sys0 = Fail::Gai {
        eai: EAI_SYSTEM,
        errno: 0,
    };
    assert_eq!(
        answer(Raw::AddrInfo(Err(sys0))),
        Answer::AddrInfo(Ok(vec![]))
    );
    let sys = Fail::Gai {
        eai: EAI_SYSTEM,
        errno: 24,
    };
    assert_eq!(answer(Raw::AddrInfo(Err(sys))), Answer::AddrInfo(Err(-24)));
    let lo = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    assert_eq!(
        answer(Raw::AddrInfo(Ok(lo.clone()))),
        Answer::AddrInfo(Ok(lo))
    );
}

/// The resolver boundary's double: what dns-lookup gives for the two names
/// native returns and lean-runtime cannot (the C buffers are dns-lookup's).
fn double(q: &Query) -> Raw {
    match q {
        Query::NameInfo(a) if a.port() == 1 => Raw::NameInfo(Err(Fail::Gai {
            eai: EAI_OVERFLOW,
            errno: 0,
        })),
        Query::NameInfo(_) => Raw::NameInfo(Err(Fail::NotUtf8)),
        Query::AddrInfo { .. } => Raw::AddrInfo(Ok(vec![])),
    }
}

/// LNET-01: a host name of exactly 1024 bytes (an `/etc/hosts` entry).
/// glibc's `NI_MAXHOST` is 1025 and libuv's buffer that size, so native
/// returns the name; dns-lookup's buffer has 1024 bytes, so glibc answers
/// `EAI_OVERFLOW`, which Lean shows as "argument buffer overflow (error
/// code: 3009)". LNET-02: a name that is not UTF-8: native decodes it
/// lossily (`caf\u{fffd}`), dns-lookup refuses it, shown as `EAI_FAIL`
/// ("permanent failure (error code: 3004)"). A program case would need such
/// entries in `/etc/hosts`; the double gives dns-lookup's answers instead.
#[test]
fn lnet_01_02_name_info_through_the_double() {
    let a1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
    let a2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2);
    let e = |a: &SocketAddr| match answer(double(&Query::NameInfo(*a))) {
        Answer::NameInfo(Err(c)) => uv_error(c),
        _ => unreachable!(),
    };
    assert_eq!(
        e(&a1),
        IoError::OtherError(3009, "argument buffer overflow".into())
    );
    assert_eq!(
        e(&a2),
        IoError::OtherError(3004, "permanent failure".into())
    );
    assert_eq!(translate_eai(EAI_FAIL, 0), -3004);
}

static SLOW_STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A slow resolver: each lookup takes 300 ms.
fn slow(_: &Query) -> Raw {
    SLOW_STARTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(300));
    Raw::AddrInfo(Ok(vec![]))
}

/// The exit's wait (`uv__threadpool_cleanup`, native's 3 s with a 3 s
/// lookup): the running lookups are waited for, the queued one never
/// starts, and no answer is delivered. Native's program case cannot pin
/// this without making glibc slow (an `LD_PRELOAD` shim), so the double
/// does (case `dns_pending_at_exit` pins the outcome without the crash).
#[test]
fn exit_waits_for_running_lookups() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let pool = Pool::new(slow, 2);
    let delivered = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let d = delivered.clone();
        pool.submit(Job {
            query: Query::AddrInfo {
                host: "localhost".into(),
                service: String::new(),
                family: 0,
            },
            deliver: Box::new(move |_| {
                d.fetch_add(1, Ordering::SeqCst);
            }),
        });
    }
    let t0 = Instant::now();
    while SLOW_STARTED.load(Ordering::SeqCst) < 2 {
        assert!(t0.elapsed() < Duration::from_secs(10), "the lookups start");
        std::thread::sleep(Duration::from_millis(1));
    }
    let t1 = Instant::now();
    pool.shutdown();
    assert!(t1.elapsed() >= Duration::from_millis(150), "the exit waits");
    assert_eq!(
        SLOW_STARTED.load(Ordering::SeqCst),
        2,
        "the queued one never starts"
    );
    assert_eq!(delivered.load(Ordering::SeqCst), 0, "no answer after exit");
    // again: nothing
    pool.shutdown();
}

static FRESH_STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A slow resolver of its own (300 ms per lookup), for the next test.
fn slow_fresh(_: &Query) -> Raw {
    FRESH_STARTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(300));
    Raw::AddrInfo(Ok(vec![]))
}

/// Review RNET-08: lookups submitted right before the exit, to helpers just
/// started that have not taken them yet. Natively the first two are taken at
/// once (fewer than two run), so the exit waits for both; the third is not
/// started. Here the first two are due, so they run even after the exit
/// began; none is delivered.
#[test]
fn exit_runs_the_due_lookups_of_fresh_helpers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let pool = Pool::new(slow_fresh, 2);
    let delivered = Arc::new(AtomicUsize::new(0));
    let t0 = Instant::now();
    for _ in 0..3 {
        let d = delivered.clone();
        pool.submit(Job {
            query: Query::AddrInfo {
                host: "localhost".into(),
                service: String::new(),
                family: 0,
            },
            deliver: Box::new(move |_| {
                d.fetch_add(1, Ordering::SeqCst);
            }),
        });
    }
    pool.shutdown();
    assert_eq!(
        FRESH_STARTED.load(Ordering::SeqCst),
        2,
        "the two due lookups run"
    );
    assert!(
        t0.elapsed() >= Duration::from_millis(300),
        "and the exit waits for them"
    );
    assert_eq!(delivered.load(Ordering::SeqCst), 0, "no answer after exit");
}

/// A scheduler for a test's thread whose contexts never suspend, as in
/// `sched`'s own unit tests: the loop context's callbacks run to their end.
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
struct NoSuspend;

#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
impl sched::Glue for NoSuspend {
    fn suspend(&self, _: sched::Suspend<'_>) {
        panic!("net's unit tests never suspend a context");
    }
}

#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
fn start_loop() {
    sched::start_with(Rc::new(NoSuspend), 2, 1 << 20);
}

/// How many loop callbacks of this thread found their socket.
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
fn callbacks_run() -> u32 {
    CALLBACKS_RUN.with(|n| n.get())
}

/// A socket bound by `make` to a loopback port outside the kernel's
/// ephemeral range (`/proc/sys/net/ipv4/ip_local_port_range`), and its
/// address, for a test that drops the socket and binds the same port again
/// at once: a parallel test's bind to port 0 is handed ports from that range
/// only, so it cannot take this one in between (with port 0 here, it could:
/// the freed port goes back to the range). The ports below the range, from
/// 20000 up to 30000 where the range allows it, are tried from an offset of
/// the process and the call, until `make` succeeds (another process may hold
/// one).
pub(super) fn bound_outside_ephemeral<T>(
    mut make: impl FnMut(SocketAddr) -> Option<T>,
) -> (T, SocketAddr) {
    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let range_start = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<u32>().ok())
        .unwrap_or(32768);
    let hi = range_start.min(30000);
    let lo = if hi > 21000 { 20000 } else { 1024 };
    assert!(hi > lo, "no port below the ephemeral range ({range_start})");
    let span = hi - lo;
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let start = (std::process::id().wrapping_mul(7919) ^ call.wrapping_mul(104_729)) % span;
    for k in 0..span {
        let port = lo + (start + k) % span;
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port as u16);
        if let Some(x) = make(addr) {
            return (x, addr);
        }
    }
    panic!("no free port in {lo}..{hi}");
}

/// The inode of the socket bound to loopback `port` in `/proc/net/<table>`
/// (`tcp`, `udp`), as the kernel lists it.
pub(super) fn socket_inode(table: &str, port: u16) -> Option<u64> {
    let local = format!("0100007F:{port:04X}");
    std::fs::read_to_string(format!("/proc/net/{table}"))
        .ok()?
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.get(1) == Some(&local.as_str()))
        .and_then(|f| f.get(9)?.parse().ok())
}

/// Whether a descriptor of this process refers to the socket with this
/// inode (`/proc/self/fd`).
pub(super) fn fd_open_to(inode: u64) -> bool {
    let target = format!("socket:[{inode}]");
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|d| std::fs::read_link(d.ok()?.path()).ok())
        .any(|p| p.as_os_str() == target.as_str())
}

/// Bind the dropped socket's port again, with plain sleeps between tries
/// (no scheduling point, so the loop does not run meanwhile): this
/// process's descriptor is closed already (`fd_open_to`), but a child that
/// another test thread is spawning holds a copy of the descriptor table
/// until it execs (`posix_spawn`'s clone), which keeps the socket bound for
/// a moment, as it would natively.
pub(super) fn bind_again<T>(bind: impl Fn() -> std::io::Result<T>) -> T {
    let t0 = std::time::Instant::now();
    loop {
        match bind() {
            Ok(x) => return x,
            Err(e) if t0.elapsed() > std::time::Duration::from_secs(5) => {
                panic!("the port is free once this process closed it: {e}")
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
}

/// AR-12: a socket dropped while it is watched (a listening socket with a
/// connection waiting, so its descriptor is ready) closes its descriptor at
/// once, as Lean's finalizer does: the watch ends with it, the port can be
/// bound again at once, and no callback of the socket runs.
#[test]
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
#[cfg_attr(miri, ignore)]
fn a_socket_dropped_while_watched_closes_at_once() {
    start_loop();
    // a port no parallel test's bind to port 0 can take once it is free
    // (libuv reports a bind's EADDRINUSE at the listen)
    let (t, addr) = bound_outside_ephemeral(|a| {
        let t = TcpSocket::new().ok()?;
        t.bind(a).ok()?;
        t.listen(4).ok()?;
        Some(t)
    });
    assert_eq!(t.sock_name().unwrap(), addr);
    let _client = std::net::TcpStream::connect(addr).unwrap();
    assert!(sched::io_cooperative(), "the socket is watched");
    let inode = socket_inode("tcp", addr.port()).expect("the listening socket");
    assert!(fd_open_to(inode));
    let before = callbacks_run();
    drop(t);
    assert!(!sched::io_cooperative(), "the watch ended with the socket");
    assert!(!fd_open_to(inode), "the descriptor is closed at once");
    bind_again(|| std::net::TcpListener::bind(addr));
    sched::sleep_ms(5);
    assert_eq!(callbacks_run(), before, "no callback of the socket ran");
    sched::finish();
}

/// AR-12: a socket dropped while a `uv__io_feed` is due, with nothing
/// pending (as a write that finished on the loop leaves it), closes its
/// descriptor at once; the feed then comes due, finds no socket and runs
/// nothing.
#[test]
#[cfg(not(all(feature = "threads", not(feature = "sched"))))]
#[cfg_attr(miri, ignore)]
fn a_socket_dropped_with_a_feed_due_closes_at_once() {
    use std::io::Read;
    start_loop();
    let lo = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    // TCP: a connected socket, nothing pending
    let l = std::net::TcpListener::bind(lo).unwrap();
    let t = TcpSocket::new().unwrap();
    let p = sched::promise_new().unwrap();
    t.connect(l.local_addr().unwrap(), move |r| {
        r.unwrap();
        sched::resolve(p, || {});
    })
    .unwrap();
    let (mut peer, _) = l.accept().unwrap();
    sched::wait(p);
    assert!(
        callbacks_run() >= 1,
        "the connect's callback found its socket"
    );
    assert!(!sched::io_cooperative(), "nothing is pending or watched");
    t.feed_for_tests();
    assert!(sched::io_cooperative(), "the feed is due");
    let before = callbacks_run();
    drop(t);
    // the descriptor is closed: the peer reads the end of the stream now,
    // with the loop not run since (a plain read, no scheduling point)
    peer.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
    assert!(sched::io_cooperative(), "the feed is still due");
    sched::sleep_ms(5);
    assert!(!sched::io_cooperative(), "the feed ran");
    assert_eq!(callbacks_run(), before, "no callback of the socket ran");
    // UDP: a bound socket, on a port no parallel test's bind to port 0 can
    // take once it is free
    let (u, addr) = bound_outside_ephemeral(|a| {
        let u = UdpSocket::new().ok()?;
        u.bind(a).ok()?;
        Some(u)
    });
    assert_eq!(u.sock_name().unwrap(), addr);
    let inode = socket_inode("udp", addr.port()).expect("the bound socket");
    assert!(fd_open_to(inode));
    u.feed_for_tests();
    drop(u);
    assert!(!fd_open_to(inode), "the descriptor is closed at once");
    // without `SO_REUSEADDR`: refused while the socket is open
    bind_again(|| std::net::UdpSocket::bind(addr));
    sched::sleep_ms(5);
    assert!(!sched::io_cooperative(), "the feed ran");
    assert_eq!(callbacks_run(), before, "no callback of the socket ran");
    sched::finish();
}
