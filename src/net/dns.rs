//! `Std.Internal.UV.DNS` (Lean 4.34.0 `src/runtime/uv/dns.cpp`):
//! `getAddrInfo` and `getNameInfo` over glibc's `getaddrinfo` and
//! `getnameinfo` (through the `dns-lookup` crate), with libuv 1.48's checks,
//! hints and error codes (`src/unix/getaddrinfo.c`, `getnameinfo.c`).
//!
//! Natively libuv runs each lookup on its thread pool (at most two slow
//! lookups at a time) and calls back on the loop thread. Here two helper
//! threads, started by the first lookup, make the C calls; they get plain
//! data (the host, service and family, or the address) and send back plain
//! data (the addresses or names, or libuv's error code), then wake the loop
//! through the loop's eventfd (native's async descriptor, `io::startup`; one
//! of the crate's own if the glue did not open native's). The loop context
//! resolves the promise. No Lean value crosses threads, and the helpers are
//! not Lean-visible parallelism: they only wait in the C library. At exit
//! the process waits for the lookups in progress and drops their answers,
//! as natively (libuv's `uv_library_shutdown` destructor joins its thread
//! pool), without native's crash (LB-27): `exit_wait`, from
//! `io::exit::exit_flush`.
//!
//! What a Lean program sees (cases `tests/cases/net/dns_*`):
//! - a host or service with a byte outside `[A-Za-z0-9._~:/+@=,%-]` fails at
//!   once with `invalidArgument` ("name is not ASCII", "service is not
//!   ASCII"), as do an empty host and a host of 256 bytes or more (libuv's
//!   IDNA conversion: `EINVAL`);
//! - the hints are the family (0 any, 1 IPv4, 2 IPv6, other values any),
//!   socket type 0 and protocol 0, so glibc gives one entry per socket type:
//!   each address comes 3 times for a numeric or empty service (TCP, UDP,
//!   raw), and once per protocol `/etc/services` names for a named one;
//! - a failure is the promise's error, libuv's `UV_EAI_*` code decoded by
//!   Lean (`otherError`, "unknown node or service (error code: 3008)" for
//!   `EAI_NONAME`).

use super::{uv_error, UV_EINVAL};
use crate::io::IoError;
use crate::sched;
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

/// A lookup for a helper thread: plain data only.
#[derive(Clone, Debug)]
pub(crate) enum Query {
    AddrInfo {
        host: String,
        service: String,
        family: i32,
    },
    NameInfo(SocketAddr),
}

/// Why a C call failed, as dns-lookup reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fail {
    /// glibc's `EAI_*` code, and `errno` for `EAI_SYSTEM`.
    Gai { eai: i32, errno: i32 },
    /// `getnameinfo` gave a name that is not UTF-8 (dns-lookup refuses it;
    /// native decodes it lossily: LNET-02).
    NotUtf8,
}

/// What the C library answered: the resolver boundary's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Raw {
    AddrInfo(Result<Vec<IpAddr>, Fail>),
    NameInfo(Result<(String, String), Fail>),
}

/// The resolver boundary: glibc's calls ([`glibc`]); the unit tests put a
/// test double here.
pub(crate) type Resolver = fn(&Query) -> Raw;

/// A lookup's answer for the loop: plain data, or libuv's error code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    AddrInfo(Result<Vec<IpAddr>, i32>),
    NameInfo(Result<(String, String), i32>),
}

/// A lookup's id and answer.
type Reply = (u64, Answer);

/// A job of the pool: the lookup, and where its answer goes (a channel to
/// the loop and the loop's wake-up, or a test's channel).
pub(crate) struct Job {
    pub(crate) query: Query,
    pub(crate) deliver: Box<dyn FnOnce(Answer) + Send>,
}

/// How many lookups libuv runs at once: its slow-work threshold, `(4 + 1) /
/// 2` for its pool of 4 threads.
const SLOW_JOBS: usize = 2;

#[derive(Default)]
struct State {
    queue: VecDeque<Job>,
    /// Running jobs.
    running: usize,
    /// The first `due` queued jobs are the ones libuv would be running now:
    /// natively a lookup is taken at once while fewer than `SLOW_JOBS` run,
    /// so it has started before the exit, even when a helper here has not
    /// taken it yet (review RNET-08).
    due: usize,
    /// Set at exit (`uv__threadpool_cleanup`): no job that is not due
    /// starts, and the answers are dropped.
    stopping: bool,
    threads: Vec<JoinHandle<()>>,
}

impl State {
    /// While the pool runs: the queued jobs libuv would have started.
    fn refresh_due(&mut self) {
        if !self.stopping {
            self.due = self.queue.len().min(SLOW_JOBS.saturating_sub(self.running));
        }
    }
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    resolve: Resolver,
}

/// libuv's thread pool, for lookups: `threads` helper threads, at most
/// [`SLOW_JOBS`] lookups at once, as libuv runs them. A thread that cannot be
/// started is libuv's `abort()` (`init_threads`).
pub(crate) struct Pool(Arc<Shared>);

impl Pool {
    pub(crate) fn new(resolve: Resolver, threads: usize) -> Pool {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            resolve,
        });
        for i in 0..threads {
            let sh = shared.clone();
            match std::thread::Builder::new()
                .name(format!("lean-runtime-dns-{i}"))
                .spawn(move || worker(&sh))
            {
                Ok(h) => lock(&shared.state).threads.push(h),
                Err(_) => std::process::abort(),
            }
        }
        Pool(shared)
    }

    pub(crate) fn submit(&self, job: Job) {
        let mut st = lock(&self.0.state);
        st.queue.push_back(job);
        st.refresh_due();
        drop(st);
        self.0.cond.notify_one();
    }

    /// `uv__threadpool_cleanup` (run at exit by libuv's
    /// `uv_library_shutdown` destructor): the lookups libuv would be running
    /// (the running ones, and the due ones a helper has not taken yet) run to
    /// their end and are waited for (the threads are joined); no other
    /// queued lookup starts; every answer is dropped. Again: nothing.
    pub(crate) fn shutdown(&self) {
        let threads = {
            let mut st = lock(&self.0.state);
            st.stopping = true;
            std::mem::take(&mut st.threads)
        };
        self.0.cond.notify_all();
        for t in threads {
            let _ = t.join();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn worker(sh: &Shared) {
    loop {
        let job = {
            let mut st = lock(&sh.state);
            loop {
                if st.stopping && st.due == 0 {
                    return;
                }
                if let Some(j) = st.queue.pop_front() {
                    // the queue's front is due whenever a helper is free
                    st.due = st.due.saturating_sub(1);
                    st.running += 1;
                    break j;
                }
                st = sh.cond.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };
        let answer = answer((sh.resolve)(&job.query));
        let stopping = {
            let mut st = lock(&sh.state);
            st.running -= 1;
            st.refresh_due();
            st.stopping
        };
        if !stopping {
            (job.deliver)(answer);
        }
    }
}

/// The lookups of the program (started by the first one).
static POOL: OnceLock<Pool> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(|| Pool::new(glibc, 2))
}

/// At exit (C's `exit`, between libc++'s flush of `std::cout` and glibc's
/// flush of every `FILE`, as libuv's destructor runs there): wait for the
/// running lookups, start no queued one, drop their answers (LB-27: never
/// delivered after `main`). Nothing when no lookup was ever made.
pub(crate) fn exit_wait() {
    if let Some(p) = POOL.get() {
        p.shutdown();
    }
}

// glibc's `EAI_*` codes (`<netdb.h>`, the `__USE_GNU` ones included).
pub(crate) const EAI_BADFLAGS: i32 = -1;
pub(crate) const EAI_NONAME: i32 = -2;
pub(crate) const EAI_AGAIN: i32 = -3;
pub(crate) const EAI_FAIL: i32 = -4;
pub(crate) const EAI_NODATA: i32 = -5;
pub(crate) const EAI_FAMILY: i32 = -6;
pub(crate) const EAI_SOCKTYPE: i32 = -7;
pub(crate) const EAI_SERVICE: i32 = -8;
pub(crate) const EAI_ADDRFAMILY: i32 = -9;
pub(crate) const EAI_MEMORY: i32 = -10;
pub(crate) const EAI_SYSTEM: i32 = -11;
pub(crate) const EAI_OVERFLOW: i32 = -12;
pub(crate) const EAI_CANCELED: i32 = -101;

/// `uv__getaddrinfo_translate_error`: libuv's code of an `EAI_*` code
/// (`UV_EAI_ADDRFAMILY` is -3000, ...); `EAI_SYSTEM` is the `errno` (0 when
/// `errno` is 0, which libuv then takes for success). libuv aborts on a
/// code it does not know (glibc returns none here).
pub(crate) fn translate_eai(eai: i32, errno: i32) -> i32 {
    match eai {
        0 => 0,
        EAI_ADDRFAMILY => -3000,
        EAI_AGAIN => -3001,
        EAI_BADFLAGS => -3002,
        EAI_CANCELED => -3003,
        EAI_FAIL => -3004,
        EAI_FAMILY => -3005,
        EAI_MEMORY => -3006,
        EAI_NODATA => -3007,
        EAI_NONAME => -3008,
        EAI_OVERFLOW => -3009,
        EAI_SERVICE => -3010,
        EAI_SOCKTYPE => -3011,
        EAI_SYSTEM => -errno,
        _ => std::process::abort(),
    }
}

/// libuv's code of a failure. `NotUtf8` (LNET-02: native returns the name
/// decoded lossily) is reported as `EAI_FAIL`.
fn code(f: Fail) -> i32 {
    match f {
        Fail::Gai { eai, errno } => translate_eai(eai, errno),
        Fail::NotUtf8 => translate_eai(EAI_FAIL, 0),
    }
}

/// libuv's and Lean's reading of the C library's answer. A failure whose
/// code is 0 (`EAI_SYSTEM` with `errno` 0) is libuv's success:
/// `getAddrInfo` walks an empty list (`ok #[]`); `getNameInfo` would read
/// buffers glibc did not fill (undefined natively), and is left an error
/// here ("unknown system error 0").
pub(crate) fn answer(raw: Raw) -> Answer {
    match raw {
        Raw::AddrInfo(Ok(v)) => Answer::AddrInfo(Ok(v)),
        Raw::AddrInfo(Err(f)) => Answer::AddrInfo(match code(f) {
            0 => Ok(Vec::new()),
            c => Err(c),
        }),
        Raw::NameInfo(r) => Answer::NameInfo(r.map_err(code)),
    }
}

/// dns-lookup's error as a [`Fail`]. Its error number is glibc's code, 0 for
/// its own failures (a name that is not UTF-8; an interior NUL, which Lean's
/// checks let none through).
fn fail(e: dns_lookup::LookupError) -> Fail {
    let eai = e.error_num();
    if eai == 0 {
        return Fail::NotUtf8;
    }
    let errno = if eai == EAI_SYSTEM {
        // dns-lookup reads `errno` right after the call, on this thread
        std::io::Error::from(e).raw_os_error().unwrap_or(0)
    } else {
        0
    };
    Fail::Gai { eai, errno }
}

/// The resolver: glibc's `getaddrinfo` (libuv's hints: the family, socket
/// type 0, protocol 0; every entry's address in glibc's order, Lean skipping
/// entries of other families, which glibc gives none of) and `getnameinfo`
/// (flags 0), on a helper thread.
pub(crate) fn glibc(q: &Query) -> Raw {
    match q {
        Query::AddrInfo {
            host,
            service,
            family,
        } => {
            let hints = dns_lookup::AddrInfoHints {
                flags: 0,
                address: *family,
                socktype: 0,
                protocol: 0,
            };
            Raw::AddrInfo(
                dns_lookup::getaddrinfo(Some(host), Some(service), Some(hints))
                    .map(|it| {
                        it.filter_map(|r| r.ok())
                            .map(|ai| ai.sockaddr.ip())
                            .collect()
                    })
                    .map_err(fail),
            )
        }
        Query::NameInfo(a) => Raw::NameInfo(dns_lookup::getnameinfo(a, 0).map_err(fail)),
    }
}

/// `is_safe_ascii_str` (dns.cpp): the bytes Lean lets through.
pub(crate) fn safe_ascii(s: &[u8]) -> bool {
    s.iter().all(|&c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'-' | b'_' | b'.' | b':' | b'/' | b'+' | b'~' | b'@' | b'=' | b',' | b'%'
            )
    })
}

/// `uv__idna_toascii`'s answer for an ASCII host (Lean lets no other
/// through): the host itself, unless it is empty or does not fit libuv's
/// 256-byte buffer with its NUL (`UV_EINVAL`).
pub(crate) fn idna_ok(host: &[u8]) -> bool {
    !host.is_empty() && host.len() < 256
}

/// The loop's side: the promises of the lookups in progress, and the watch
/// of the wake-up descriptor while there are any.
#[derive(Default)]
struct Client {
    next: u64,
    pending: HashMap<u64, Box<dyn FnOnce(Answer)>>,
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

/// Submit `query`; `done` runs on the loop context with the answer.
fn submit(query: Query, done: Box<dyn FnOnce(Answer)>) -> Result<(), IoError> {
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
        let cb: Rc<dyn Fn(sched::Ready)> = Rc::new(move |_| deliver(wake));
        let w = sched::watch(wake, sched::Interest::READ, cb).ok();
        CLIENT.with(|c| c.borrow_mut().watch = w);
    }
    pool().submit(Job {
        query,
        deliver: Box::new(move |answer| {
            if reply.send((id, answer)).is_ok() {
                // `uv_async_send`: wake the loop
                let _ = rustix::io::write(wake, &1u64.to_ne_bytes());
            }
        }),
    });
    Ok(())
}

/// The wake-up descriptor is readable: resolve the answered lookups, in the
/// order the helpers answered; stop watching once none is left.
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

/// `DNS.getAddrInfo` (`lean_uv_dns_get_info`, `uv_getaddrinfo`): the
/// addresses of `host` for `service` and `family` (Lean's 0 any, 1 IPv4, 2
/// IPv6). The checks fail at once; `done` gets the lookup's outcome on the
/// loop.
pub fn get_addr_info(
    host: &str,
    service: &str,
    family: u8,
    done: impl FnOnce(Result<Vec<IpAddr>, IoError>) + 'static,
) -> Result<(), IoError> {
    crate::sched::ensure_started();
    crate::io::effect_point();
    if !safe_ascii(host.as_bytes()) {
        return Err(IoError::InvalidArgument(
            None,
            22,
            "name is not ASCII".to_owned(),
        ));
    }
    if !safe_ascii(service.as_bytes()) {
        return Err(IoError::InvalidArgument(
            None,
            22,
            "service is not ASCII".to_owned(),
        ));
    }
    super::loop_lock();
    if !idna_ok(host.as_bytes()) {
        return Err(uv_error(UV_EINVAL));
    }
    let family = match family {
        1 => 2,  // PF_INET
        2 => 10, // PF_INET6
        _ => 0,  // PF_UNSPEC
    };
    submit(
        Query::AddrInfo {
            host: host.to_owned(),
            service: service.to_owned(),
            family,
        },
        Box::new(move |a| {
            if let Answer::AddrInfo(r) = a {
                done(r.map_err(uv_error))
            }
        }),
    )
}

/// `DNS.getNameInfo` (`lean_uv_dns_get_name`, `uv_getnameinfo(.., 0)`): the
/// host and service names of `addr` (no flow label, no scope); `done` gets
/// them on the loop.
pub fn get_name_info(
    addr: SocketAddr,
    done: impl FnOnce(Result<(String, String), IoError>) + 'static,
) -> Result<(), IoError> {
    crate::sched::ensure_started();
    crate::io::effect_point();
    super::loop_lock();
    let addr = match addr {
        SocketAddr::V6(a) => SocketAddr::V6(std::net::SocketAddrV6::new(*a.ip(), a.port(), 0, 0)),
        a => a,
    };
    submit(
        Query::NameInfo(addr),
        Box::new(move |a| {
            if let Answer::NameInfo(r) = a {
                done(r.map_err(uv_error))
            }
        }),
    )
}
