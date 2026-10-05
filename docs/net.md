# Networking (`net`)

`net` (feature `net`, which turns on `io` and needs a scheduler, `sched` or
`threads`: alone it is a compile error) is Lean 4.34.0's networking
externs, for a translator that admits the module, on the single-thread
scheduler's event loop or, in threads mode, on `sched::uv`'s loop thread
("Threads mode" below):

| Lean | Extern (`src/runtime/uv/`) | Here |
|---|---|---|
| `Std.Internal.UV.TCP.Socket` | `lean_uv_tcp_*` (16), `tcp.cpp` | `net::tcp::TcpSocket` |
| `Std.Internal.UV.UDP.Socket` | `lean_uv_udp_*` (15), `udp.cpp` | `net::udp::UdpSocket` |
| `Std.Internal.UV.DNS` | `lean_uv_dns_get_info`, `lean_uv_dns_get_name`, `dns.cpp` | `net::dns` |
| `Std.Net.interfaceAddresses` | `lean_uv_interface_addresses`, `net_addr.cpp` | `net::iface` |

`Std.Async.TCP`, `UDP` and `DNS` are Lean code over these; a translator
compiles them. The text forms of addresses (`IPv4Addr.ofString`,
`toString`, ...) need no loop and are in `semantics::net`.

## Files

| File | What |
|---|---|
| `src/net/mod.rs` | The model, the glue's types (`Done`, `MaybeSend`, `SendData`, `RecvBuf`, `RecvTarget`), the socket's handle (`Handle`), libuv's io watcher and `uv__io_feed` over the mode layer |
| `src/net/mode_st.rs`, `src/net/mode_mt.rs` | the mode layer, one per scheduler: the socket's cell and counted reference, the registry of open sockets, the io watcher's and `uv__io_feed`'s backend, the loop's lock, the DNS answers' way back to the loop |
| `src/net/tcp.rs` | libuv 1.48's stream and TCP code (`stream.c`, `tcp.c`) and Lean's `tcp.cpp` |
| `src/net/udp.rs` | libuv's `udp.c` and Lean's `udp.cpp` |
| `src/net/dns.rs` | the lookups, on two helper threads |
| `src/net/iface.rs` | `uv_interface_addresses` over `getifaddrs` |
| `src/net/tests.rs` | unit tests of the pure parts, and of a socket's lifetime on the single-thread loop |
| `src/net/tests_mt.rs` | unit tests of threads mode: the drop and cancel paths that cross threads |
| `tests/cases/net/` | the program cases, recorded natively; twins in `tests/sched-driver/src/netcases.rs`, run by both drivers (`tests/sched-driver`, and in threads mode `tests/sched-driver-mt`, 5 runs each) |

Dependencies: rustix's and nix's `net` features (sockets, socket options,
`getifaddrs`), and `dns-lookup` 2.1.1 for glibc's `getaddrinfo` and
`getnameinfo` (`UNSAFE.md`, "Dependencies").

## The model

Natively one thread runs libuv's loop for the whole program
(`event_loop.cpp`). An extern locks it, starts the operation and returns a
promise; the loop thread later finishes the operation and resolves the
promise. Here:

- **One socket mirrors one libuv handle and Lean's object around it**:
  libuv's flags (readable, writable, bound, shut, reading, end of file),
  its delayed error, its queues of writes, its connect and shutdown
  requests, a listening socket's accepted descriptor, and Lean's pending
  promises (`m_promise_read`, `m_promise_accept`, `m_promise_shutdown`).
- **The system calls are libuv's, in its order.** Where libuv makes them in
  the calling thread, the extern makes them: `bind`, `listen`, `connect`'s
  `connect(2)`, a `send`'s first write (what fits), a UDP datagram's
  `sendmmsg`, the socket options. Where libuv's loop thread makes them,
  a callback on the scheduler's loop context does: reads, the rest of a
  write, `accept4`, `shutdown(2)` after the queued writes, `SO_ERROR`
  after a connect.
- **libuv's io watcher** is a `sched::watch` of the socket's descriptor
  (the watch holds a clone of the socket's `Rc<OwnedFd>` until it ends, and
  the socket's number, not the socket: "Ownership" below),
  for what libuv waits for (`POLLIN` while reading or listening, `POLLOUT`
  while connecting or writing), changed where libuv calls
  `uv__io_start`/`uv__io_stop`. **`uv__io_feed`** (a write finished, a
  shutdown with nothing queued, a delayed connect error) is a timer due
  at once, which runs the watcher's callback with `POLLOUT` at the loop's
  next turn, as libuv's pending queue does.
- **The loop's lock.** Natively an extern takes the loop's lock
  (`event_loop_lock`), which makes the loop thread finish its iteration
  first: what became ready meanwhile is handled before the extern acts.
  Here each extern first lets the loop context run what is due
  (`sched::catch_up`, as `sched::uv`'s externs do).
- **Callbacks run on the loop context** (`docs/sched.md`, "The event
  loop"), as natively on the loop thread: they resolve the promises, so
  their waiters wake and their `sync` dependents run there.

### Threads mode

With `threads` (docs/threads.md, 0.7) the same code runs on `sched::uv`'s
loop thread, as natively on libuv's: the mode layer (`mode_mt.rs`) gives
the socket a lock in an `Arc` (a `TcpSocket` and a `UdpSocket` are `Send +
Sync`), the watches are the loop thread's epoll instance, a `uv__io_feed`
is its pending queue, and the loop's lock is `sched::uv`'s:
- **Every extern holds the loop lock** from its start to its end, as
  natively (`event_loop_lock` ... `event_loop_unlock`); when the loop
  thread holds it, the extern interrupts its iteration and waits for its
  end, so what became ready meanwhile is handled first. A socket's state
  lock is taken only under it, and never across translator code.
- **Callbacks run on the loop thread** with the loop lock held: a promise
  resolves there, its `sync` dependents run there, its waiters on other
  threads wake. An extern that resolves at once does so after it lets go
  of the loop lock, as native's (`accept` of a connection the loop already
  took); an empty `send` takes no lock.
- **Lean's finalizer** of a socket, the drop of its last handle, takes the
  loop lock on whatever thread drops it, then closes the socket at once. A
  pending operation holds the socket, so a socket dropped on a worker
  while the loop thread has a receive pending stays open until the receive
  completes there.
- **What the loop keeps is `Send`**: a `done` closure, `SendData` and
  `RecvBuf` have the bound `MaybeSend`, which is `Send` in threads mode and
  nothing in the single-thread mode.
- **DNS**: a helper hands its answer to the loop thread through the loop's
  async queue and eventfd, never through the loop lock; the loop thread
  runs the promise's `done`.
- **The order within an iteration** is libuv's: the io callbacks (the
  sockets', then the async queue's), the signal watchers last, then the
  pending callbacks and the timers.
- **After `finish`** the loop thread runs no callback, so no promise
  resolves then (LB-27), as the single-thread scheduler's loop does not
  run after `finish`.

The program cases give native's outcomes in threads mode too (5 runs
each), the two that use the network from several tasks included
(`clients_in_tasks`, `socket_across_tasks`); unit tests cover the drop and
cancel paths across threads (`src/net/tests_mt.rs`).

Example (`tcp_echo`): a client `send`s 16 MiB to a server task that sleeps
before it reads.
1. `send` writes what the socket takes (a few MiB on the loopback) and
   queues the rest; the socket's watch now waits for `POLLOUT`.
2. `shutdown` marks the socket not writable; a `send` after it fails at
   once with `EPIPE`; the shutdown waits for the queue.
3. The server reads; the loop sees `POLLOUT`, writes the next piece, and so
   on until the queue is empty; then `shutdown(SHUT_WR)`, and both
   promises resolve on the loop.
4. The server's `recv?` gets `none` after the last byte.

### The glue

The API returns crate types and plain data, and takes closures:
- **`done`**: an operation that returns a promise takes a one-shot closure
  holding the translator's promise, which resolves it with the result
  (`Result<T, IoError>`, Lean's `Except IO.Error T`). Holding it is the
  loop's `lean_inc(promise)`; dropping it uncalled (`cancelRecv`,
  `cancelAccept`) is `lean_dec`, after which the promise resolves to
  `none` only when the program drops its own last reference, as natively.
  `done` may run before the extern returns where native resolves the
  promise before returning: an empty `send`, an `accept` of a connection
  the loop already took.
- **`SendData`**: `send` keeps the translator's `Array ByteArray` (a view
  per buffer) until the write is done, as native holds the array.
- **`RecvBuf`**: `recv?`/`recv` take `alloc`, which allocates the
  translator's new `ByteArray` of the size asked for, called where native
  calls `lean_alloc_sarray(1, 0, size)` (after the check for a receive in
  progress): with Lean's checked arithmetic (`semantics::array::alloc_bytes`)
  and its internal panics (`integer overflow in runtime computation` above
  2^64 - 25, `out of memory` for a size it cannot allocate; cases
  `recv_huge_*`). The bytes are read into its storage
  (`RecvTarget::Uninit`, or a `Vec`'s spare capacity), never copied.
  `alloc` runs with nothing of the socket held (it may end the process
  through the glue's internal panic, an effect point where other contexts
  run: review RNET-01); `RecvBuf::target` and `SendData::get` run while the
  crate holds the socket's state, so they only hand out bytes and never
  call into the crate or yield.
- **Lifetime**: a `TcpSocket`/`UdpSocket` is a counted handle (`Clone`); the
  socket closes when the last one goes (Lean's finalizer, `uv_close`). A
  pending operation holds the socket, as native's `lean_inc(socket)`, so a
  socket the program dropped still finishes it ("Ownership" below).
- Errors are `IoError`s built as the externs build them
  (`lean_decode_uv_error(code, nullptr)`; Lean's own `invalidArgument`
  texts for DNS).

`tests/sched-driver/src/lnet.rs` is a complete glue, with the `Std.Async`
monad as its Lean code builds tasks.

### Ownership

Natively Lean's socket object owns the libuv handle, and the loop holds a
reference to the socket (`lean_inc`) only while one of its requests is
queued; Lean's finalizer closes the handle (`uv_close`), and with it the
descriptor, when the last reference goes. Here (AR-12):

- **The handle owns the socket.** `TcpSocket` and `UdpSocket` are an `Rc`
  (in threads mode an `Arc`) of one `net::Handle`, which owns the socket's
  state and its entry in the registry of open sockets (the thread's; in
  threads mode the process's; a number given once, never reused).
  The program's clones hold the handle, and so does each pending operation
  (a connect, a receive, a queued write or datagram, a shutdown, an
  accept), as native's `lean_inc(socket)`, until its promise is resolved
  or the operation is cancelled.
- **The loop's callbacks hold a number.** The watch of the descriptor and
  a due `uv__io_feed` hold the socket's number, never a reference. When one
  runs, it looks the socket up in the registry (`net::on_socket`) and holds
  it for the call; if the socket is no longer there, it has closed, and the
  callback does nothing. A watch's callback always finds its socket (the
  socket's close ends the watch before another callback can run); a feed
  due after the close finds nothing. There is no `Weak` in the module.
- **The close.** When the handle's last clone goes, its drop removes the
  entry, then lets go of the state, whose drop ends the watch
  (`uv__io_close`) and closes the descriptor (`uv_close`), at once: the
  port can be bound again right after. Only a loop callback running on the
  socket at that moment (its last operation's promise resolved there) holds
  the state until it returns; the close comes then, before the loop runs
  anything else. Natively the finalizer takes the loop's lock before
  `uv_close` (`tcp.cpp`, `udp.cpp`), so its close too comes no later than
  the end of the callback, and the rest of the callback does nothing
  visible: no operation is pending any more.
- **No cycle through the loop.** Nothing the loop holds refers to a socket,
  so the loop never keeps one open. The only reference cycle is a pending
  operation's (the socket holds the operation, which holds the handle), and
  it ends with the operation, as native's `lean_inc` ends with the
  request. The registry is never a socket's last owner (the handle holds
  the state while the entry exists), so removing an entry, or the registry
  itself at the thread's end, closes nothing.

Example (the state the unit test
`a_socket_dropped_with_a_feed_due_closes_at_once` makes):
1. A `send` finishes on the loop: the watch's callback finds the socket,
   writes the rest and resolves the send's promise, and the write lets go
   of its clone of the handle. The `uv__io_feed` of the write is still
   due, with the socket's number.
2. The program drops its handle, the last clone: the entry goes, and the
   descriptor closes; the peer reads the end of the stream at once.
3. The feed comes due, finds no socket, and does nothing.

## What a program sees

TCP (cases `tcp_echo`, `tcp_errors`, `tcp_v6`, `keepalive_zero_delay`,
`recv_zero_*`, `accept_parallel*`, `shutdown_*`; from several tasks,
`clients_in_tasks` and `socket_across_tasks`):
- A new socket has no descriptor until `bind`, `connect` or `listen`: then
  `getPeerName`, `getSockName` and `send` fail with `EBADF`, `recv?`,
  `waitReadable` and `shutdown` with `ENOTCONN`; `noDelay` and `keepAlive`
  succeed and apply to the descriptor when it comes (keep-alive with
  libuv's 60 s).
- `bind` sets `SO_REUSEADDR` (and clears `IPV6_V6ONLY` for IPv6). An address
  in use is reported by the next `listen`, `connect`, `getSockName` or
  `getPeerName` (libuv's delayed error). `listen` on a socket without a
  descriptor makes an IPv4 one, which the kernel binds to a free port of
  every interface.
- One `connect`, one receive (`recv?` or `waitReadable`) and one `shutdown`
  at a time: a second fails with `EALREADY` ("connection already in
  progress (error code: 114)"); a `shutdown` after one has finished fails
  with `ENOTCONN`.
- `connect` to a closed port resolves with `ECONNREFUSED`; a second
  `connect` on that socket fails with `ECONNABORTED`. A second `connect` on
  a socket whose first connect succeeded succeeds (the kernel's answer), a
  third fails with `EISCONN`.
- `recv? n` reads once, at most `n` bytes: `some` bytes, `none` at end of
  file (as often as asked), or an error. `recv? 0` waits until the socket
  is readable, then fails with `ENOBUFS` while bytes are unread (nothing is
  consumed), and gives `none` at the end of the stream (LB-26).
  `waitReadable` resolves
  `true` once the socket is readable, end of file included.
- `accept` resolves at once with a connection the loop already accepted
  (libuv accepts one as it arrives, even with no `accept` pending), else
  with the next one; on a socket that is not listening it stays pending.
  `tryAccept` gives the connection the loop accepted, or `none`.
- `keepAlive 1 d`: `SO_KEEPALIVE`, `TCP_KEEPIDLE` `d`, `TCP_KEEPINTVL` 1,
  `TCP_KEEPCNT` 10; a `d` above 32767 fails with the kernel's `EINVAL`.

UDP (cases `udp_basic`, `udp_errors`, `udp_cancel_recv_leak`,
`multicast_ipv6_long`):
- `bind` sets `SO_REUSEADDR`. `connect`, `send` with an address, `recv`,
  `waitReadable` and `setMembership` bind a socket without a descriptor to
  the wildcard address of the family, port 0.
- A `send`'s buffers are one datagram, sent at once if the socket takes it;
  its promise resolves on the loop, with the error of the send (`EMSGSIZE`
  above 65507 bytes on IPv4). `send` with an address on a connected socket
  fails with `EISCONN`, without one on an unconnected socket with
  `EDESTADDRREQ`.
- `recv n` gives the next datagram, cut to `n` bytes (the rest is lost),
  and its sender; `recv 0` fails with `ENOBUFS` once a datagram is there,
  leaving it (case `udp_recv_zero`); on a connected socket whose peer port is closed, the
  receive after a send gives `ECONNREFUSED`.
- The option setters on a socket without a descriptor fail with `EBADF`;
  `setTTL` takes 1 to 255, `setMulticastTTL` 0 to 255 (`EINVAL` otherwise);
  on a socket bound to IPv6 they set the IPv6 options.

DNS (cases `dns_localhost`, `dns_pending_at_exit`):
- The host and service must be made of `[A-Za-z0-9._~:/+@=,%-]`
  ("name is not ASCII", "service is not ASCII"), the host non-empty and
  shorter than 256 bytes (`EINVAL`): checked at the call.
- glibc's `getaddrinfo` with libuv's hints: the family, socket type 0,
  protocol 0. So each address comes once per socket type: three times for a
  numeric or empty service, once per protocol `/etc/services` lists for a
  named one. A failure is libuv's `UV_EAI_*` code on the promise
  (`otherError`: "unknown node or service (error code: 3008)").
- `getNameInfo` gives glibc's names with flags 0.

**The helper threads.** libuv runs lookups on its thread pool. Here two
threads, started by the first lookup (never at startup), call glibc. They
get plain data (host, service, family, or an address) and send back plain
data (addresses, names, or libuv's code), then wake the loop through the
loop's eventfd (native's async descriptor when the glue opened native's
startup descriptors, `io::startup`; else one of the crate's own). A watch of
that eventfd, registered while a lookup is pending, resolves the promises
on the loop context; in threads mode the answer goes through `sched::uv`'s
async queue to the loop thread, which resolves the promise. No Lean value
crosses threads. The threads are not
Lean-visible parallelism: they only wait inside the C library, and the
program's own code runs on the scheduler's one thread as before. At exit
the process waits for the lookups in progress, as natively: libuv's
`uv_library_shutdown` destructor joins its thread pool (`uv-common.c`
944-961, `threadpool.c` 167-190), between libc++'s flush of `std::cout` and
glibc's flush of the other streams. The lookups libuv would be running then
(fewer than two running take a new one at once) run to their end, even
when a helper here has not taken them yet; a lookup queued behind them does
not start; the answers are dropped (`net::dns::exit_wait`, called by
`io::exit::exit_flush`; LB-27). The unit test
`exit_waits_for_running_lookups` and
`exit_runs_the_due_lookups_of_fresh_helpers` (review RNET-08) pin the wait
with a slow test double at the resolver boundary: a program case cannot, since making glibc's lookup
slow takes an `LD_PRELOAD` shim, which the case runner does not have (with
one, native exits after 3.00 s for a 3 s lookup, leanrs's probe). Case
`dns_pending_at_exit` pins the outcome without native's crash.

Interfaces (case `iface_lo`): the IPv4 and IPv6 addresses of interfaces up
and running, in `getifaddrs`'s order, with the netmask, the loopback flag
and the hardware address (an alias `eth0:1` takes `eth0`'s).

## Where it differs from native

Native bugs not reproduced (`docs/lean-bugs.md`; each with a case whose
expected outcome is the correct one, native's in its `native` field):

| Bug | Native | Here | Case |
|---|---|---|---|
| LB-21 | A second `accept` while one is pending returns `EALREADY` with the event loop still locked: no loop callback ever runs again | The error, and the loop goes on | `accept_parallel` |
| LB-22 | `setMembership`/`setMulticastInterface` with an IPv6 address whose text has 16 characters or more abort (a 16-byte buffer) | The call goes on as for a short address | `multicast_ipv6_long` |
| LB-23 | `keepAlive 1 0` on a socket with a descriptor fails with `EPERM` (libuv 1.48's bare -1) | `EINVAL`, as Lean's docstring and libuv 1.49 say | `keepalive_zero_delay` |
| LB-24 | UDP `cancelRecv` keeps the loop's reference: the socket and its descriptor are never freed | Freed when the program drops the socket | `udp_cancel_recv_leak` |
| LB-25 | A failing TCP `shutdown` keeps a reference: the socket leaks | Nothing is taken before the checks | `tcp_shutdown_fail_leak` |
| LB-26 | `recv? 0` at the end of the stream fails with `ENOBUFS` | `none`, as the docstring says (with bytes unread: `ENOBUFS`, as natively) | `recv_zero_eof`, `recv_zero_data_eof` |
| LB-27 | A DNS lookup finishing after `main` returned crashes the exit (SIGSEGV, about 2 runs in 5: `lean_promise_resolve` on the finalized task manager) | The exit waits for the lookup, as natively, and drops its answer; exit with `main`'s status | `dns_pending_at_exit` (`hand_written`) |
| LB-28 | A `shutdown` requested while the `connect` is pending, with no write queued, never happens: the promise never resolves, no FIN, the socket is kept | The shutdown happens once the connect succeeds (behind any queued write); it fails with `ECANCELED` if the connect fails | `shutdown_during_connect` (controls `shutdown_after_queued_write`, `shutdown_after_connect`) |

Each spot is marked `LEAN-BUG LB-nn` in the source.

lean-runtime's own deviations, judged (native is right, but the crate's
safe route cannot do the same; in a case's `.toml` they would read
`deviations = { lean_runtime = "LNET-0n", leanrs = "DV2" }`):

| Id | Native | Here | Why | Test |
|---|---|---|---|---|
| LNET-01 | `getNameInfo` of an address whose host name has exactly 1024 bytes returns the name (glibc's `NI_MAXHOST` is 1025, libuv's buffer that size) | `argument buffer overflow (error code: 3009)` (`EAI_OVERFLOW`) | dns-lookup's host buffer has 1024 bytes, and a larger one needs `unsafe` | unit test `lnet_01_02_name_info_through_the_double` |
| LNET-02 | `getNameInfo` of a name that is not UTF-8 returns it decoded lossily (`caf\u{fffd}`) | `permanent failure (error code: 3004)` (`EAI_FAIL`) | dns-lookup refuses the name, and reading the C buffer ourselves needs `unsafe` | the same |

Both need entries in `/etc/hosts`, so no portable program case shows them;
the unit test gives `net::dns` dns-lookup's answers through a test double
at the resolver boundary (`dns::Resolver`).

Other differences, none visible in the cases:
- **When the loop looks** (the single-thread mode; in threads mode the loop
  thread runs as natively). Native's loop thread runs as soon as a socket is
  ready. Here the loop's callbacks run when the program blocks (a wait for
  a promise, a sleep, a blocking read), at polling and effect points (at
  most once a millisecond), or on a context of their own while other
  contexts run: a program that computes without any of these delays them.
  A race native leaves to its threads (a `tryAccept` right after a client's
  connect resolved, a second `shutdown` racing the first's completion) can
  take one of native's outcomes.
- **Thread pool**: two lookup threads, where libuv's pool has four threads
  and runs at most two slow jobs; lookups beyond two wait in order.
- **`getNameInfo` with `EAI_SYSTEM` and `errno` 0**: libuv takes it for a
  success and reads buffers glibc did not fill (undefined natively); here
  it is an error, `unknown system error 0`. (`getAddrInfo` gives `ok #[]`
  there, as natively.)
- **A UDP socket on descriptor 0, 1 or 2**: libuv's `uv__udp_close` asserts
  `fd > STDERR_FILENO` and aborts natively; here the descriptor is left
  open. Out of reach in practice: when 0 to 2 are closed at start, native's
  startup descriptors take them first (`io::startup`).
- **Not modelled**: libuv's assertions (`listen` twice while the loop holds
  an accepted descriptor), `UV_EMFILE` handling is libuv's own (its spare
  descriptor, opened by the first TCP socket on `/dev/null`).

## Costs

A program that uses no socket pays nothing: the module is behind its
feature, and nothing runs until an extern is called. A socket costs two
`Rc`s (the handle and its state), a `RefCell` and an entry in the
thread's registry (a hash map); each loop callback looks its socket up
there once; each wait for readiness is an `epoll_ctl` through the
scheduler's watch (added, changed, removed where libuv does), and each
completion one due timer. A `send` copies nothing: the translator's buffers
are written from where they are. In threads mode a socket costs two `Arc`s
and a lock instead, and its entry is in the process's registry (under a
lock). The loop thread holds the loop lock through its whole iteration,
its wait in `poll(2)` included, so nearly every extern finds it held: it
writes the eventfd, the loop thread wakes and ends its iteration, and the
lock is handed over (as natively, where `event_loop_lock` interrupts
`uv_run` the same way); only an extern made between two iterations, or on
the loop thread itself, takes it at once. A completion is one entry of the
loop's pending queue.
