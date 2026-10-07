# lean-runtime

The behaviour of Lean 4.34.0's runtime, as one Rust crate shared by two
translators of compiled Lean programs:

- **lean2rr** ([reussir-lang/lean-to-reussir](https://github.com/reussir-lang/lean-to-reussir)) translates to Reussir;
- **leanrs** translates to safe Rust.

For a readable overview with diagrams, open [site/index.html](site/index.html). (The repository is named lean-runtime-rs, as a Rust reimplementation; the crate is `lean-runtime`.)

Both must produce programs that behave exactly like Lean's own build:
the same stdout, stderr and exit code. Both therefore reimplement what
Lean's C runtime (`src/runtime/*.cpp`, `lean.h`) does. This crate holds the
part of that work that does not depend on how a translator represents Lean
values.

## What is here, and what is not

| In this crate | In each translator's own glue |
|---|---|
| `semantics`: hashing, float and character formatting, `UIntN`/`IntN` rows, `Nat`/`Int` rules over a big-number trait, string-position algorithms on UTF-8 bytes, array edge rules, IP address text | The representation of Lean values (`Nat` words, strings, arrays, user types) |
| `io` (feature `io`): glibc `FILE` buffering, files and handles, directories, environment, clock, `errno` to `IO.Error`, `main` on a thread with Lean's stack (`io::startup::run_main`, with a scheduler); with the feature `proc-title` (it turns on `io`), `setProcessTitle`'s write into the arguments' memory, a native quirk written with `unsafe` (without it, `setProcessTitle` fails with `ENOBUFS`); with the feature `startup-fds` (it turns on `io`), an ELF constructor that opens native Lean's startup descriptors before Rust's runtime starts, a native quirk written with `unsafe` (without it, a glue writes that constructor itself) | The memory protocol: reference counting, ownership, freeing |
| `sched` (feature `sched`): deferred tasks run as coroutines, yield points, promises, `Std.Sync`, Lean's exit behaviour, and a lazy start (`sched::start_lazy`: the scheduler built at the first task; single-thread only, threads mode starts eagerly); or, with the feature `threads` instead (threads mode, `docs/threads.md`), Lean's task manager on real threads, with the same functions and `Send` bounds; with the feature `stack-overflow` (with `sched` or `threads`), Lean's stack-overflow report for the scheduler's stacks, a native quirk written with `unsafe` (without it, a task's stack overflow is a plain SIGSEGV, status 139) | Hot paths on the translator's own types (the `Nat` fast path, in-place string and array updates) |
| `net` (feature `net`, with `io`; it needs a scheduler, `sched` or `threads`, and turns none on): TCP, UDP, DNS and interface addresses (`Std.Internal.UV`, `Std.Net`) on the scheduler's event loop, in threads mode on `sched::uv`'s loop thread | Its promises and `ByteArray`s (the crate calls back to resolve and to allocate them) |

Functions take views (`&[u8]`, `&str`) and plain data (`u64`, `f64`), and
return plain data or write into a buffer the caller supplies, so either
translator can wrap them without converting values.

## Status

`semantics` has hashing, `Float`/`Float32` formatting and conversions, the
libm rows, the fixed-width integer rows and the `String` position functions
(batch 1), and the `Nat`/`Int` rules over the big-number traits of
`semantics::bignum`, the array edge rules, the panic texts and exit
statuses, and the text of leaf values (batch 2), and `String.Pos.Raw.set`,
`ByteArray.validateUTF8`, the toolchain's build facts and the text forms of
IP addresses (batch 3), checked by the rows in
`tests/cases/*/*.rows.toml`
(expected values from native Lean 4.34.0, `scripts/gen_rows.py`), with one
micro-benchmark per public function and its native-Lean twin in `benches/`
(`scripts/gen_benches.py`; not timed yet).

`io` has its first batch (feature `io`): the `IO.Error` mirror and its
decoding from `errno`, glibc's `FILE` model, handles and the standard
streams, the exit sequence, the file system, the environment, the clock and
the debug primitives, with bench pairs for its hot paths in `benches/io/`
(not timed yet). Its second batch adds child processes (over
`posix_spawn`), `timeit` and `Std.Time`'s clock, temporary files, the
`Std.Internal.UV.System` queries and the redirection of the standard
streams; `tests/io2_cases.rs` runs a Rust twin of each of its program cases
(`tests/cases/{process,temp,time,uvsys,streams}`) through the case checker,
and `benches/io/` has pairs for spawning and `IO.Process.output`. See
`src/io/mod.rs`.

A small `io` batch, `quirks-1`, adds `IO.initializing` and `allocprof`,
and makes `setProcessTitle` write the title into the arguments' memory, so
`/proc/self/cmdline` shows it as natively. That write is the crate's first
`unsafe` item, a native quirk in `src/io/argv_title.rs` (`UNSAFE.md`,
`docs/native-quirks.md`), compiled only with the feature `proc-title`
(lean2rr enables it; without it, `setProcessTitle` fails with `ENOBUFS`
and `getProcessTitle` gives `argv[0]`). The next, `quirks-2`, makes the
two io_uring rings among the startup descriptors real rings, made and
mapped as libuv makes them, through the io-uring crate, in place of epoll
stand-ins.

`sched` has its first batch (feature `sched`): deferred tasks on corosensei
contexts, the yield points, promises, `Std.Sync`'s primitives and Lean's
exit behaviour. Every case of `tests/cases/tasks` and `tests/cases/sync`
passes as a Rust program over it (`tests/sched-driver`). A translator's glue
has one `unsafe` step, whose soundness argument is in `docs/sched.md`.
Its second batch, sched-io, makes blocking IO cooperate with the tasks (a
read of an empty pipe, a write into a full one, `flock`, `waitpid` let the
other tasks run, as natively only their own thread waits) and adds the
scheduler's event loop (epoll, timers, descriptor watches), with
`Std.Internal.UV`'s loop, timers and signals on it (`sched::uv`); the cases
of `tests/cases/taskio` and `uvloop` and the io cases with tasks pass
through the driver. Its third batch, sched-2, covers the last Task/Promise
symbols: `Promise.result?` and `Task.pure` are glue (`docs/sched.md`, "The
glue"), and `Option.getOrBlock!`, behind `Promise.result!`, is
`sched::option_get_or_block`. A task's waiters wake at the end of the
first walk of dependents that ends after its value is set (its own, a
nested one or any other referenced task's), as natively, and also where
`Promise.result!`'s permanent block would lose the wakeup (LB-32). Its
fourth batch, sched-3, makes Lean's stack-overflow report the crate's,
behind the feature `stack-overflow` (`sched::install_stack_overflow_handler`,
a native quirk in `src/sched/stack_overflow.rs`; AR-11; without it, a task
that overflows its context's stack ends with a plain SIGSEGV), and lets a
waiter or a poller run no task on its own stack but the one a free worker
would start now, first come, first served, with `IO.waitAny` keeping its
worker (AR-9, AR-10). A small fifth, sched-4, counts the workers as
native does in two more places: a pool task that waits forever on itself
frees its worker, as native's `wait_for` does (AR-15), and a pool task's
worker stays busy for its walk of `sync` dependents, whose waits never
free it (AR-16); and it records lean-runtime's first known difference of
the deferred model, LSCHED-01 (`docs/sched.md`, "Known differences from
native"). In fixes-3, a pure task a worker has started keeps that worker
until it runs, as natively (AR-25), with two more known differences,
LSCHED-02 and LSCHED-03.

Threads mode (feature `threads`, which excludes `sched`) has its
first batch, T1: `sched::mt`, Lean 4.34.0's task manager on real threads
(a pool of `LEAN_NUM_THREADS` workers, a thread per dedicated task, one
more worker while a pool task waits, one lock), promises, `Std.Sync`, the
4.35 rule for refs (`sched::Ref`) and the exit without LB-13, re-exported as
`sched` with the single-thread scheduler's names. With `io`, io takes its
plain blocking path. Its unit tests run under Miri too. The second batch,
T2, added io's own items in threads mode: the working directory's lock
rule for path lookups where `unshare(CLONE_FS)` is refused (review
RT1-04, RT2-03), and `sched::uv`, `Std.Internal.UV`'s loop on a thread of
its own as natively, with the single-thread module's names. In both modes
a pool worker keeps its standard streams and `errno` from one task to the
next, as natively (review AR-24). The io and uvloop cases' twins run inside
tasks in a threads build. The third batch, T3, runs the task, sync, refs
and taskio cases in threads mode through a second driver
(`tests/sched-driver-mt`), 5 runs each, with the single-thread driver's
ports of the same cases, and adds four cases that need real contention,
recorded natively. The batch net-threads (N) runs `net` in threads mode:
the sockets and lookups on `sched::uv`'s loop thread, as natively, every
extern and a socket's finalizer under the loop lock, one source for both
modes (a mode layer, `src/net/mode_st.rs` and `mode_mt.rs`); the cases of
`tests/cases/net` pass through the second driver too, 5 runs each. The
translators' threads modes come later. See `docs/threads.md`.

`net` (feature `net`; it turns on `io`, and needs `sched` or `threads`) has
Lean's networking externs: `Std.Internal.UV.TCP` and `UDP` (libuv 1.48's
stream and UDP code over non-blocking sockets, on the scheduler's event
loop, or in threads mode on `sched::uv`'s loop thread), `DNS` (glibc's
`getaddrinfo` and `getnameinfo` through dns-lookup, on two helper threads)
and `Std.Net.interfaceAddresses`. The cases of `tests/cases/net` pass
through the driver; ten native bugs are not reproduced (LB-21 to LB-28,
LB-50 and LB-51).
Its second batch, net-2, holds no `Weak` reference: the loop's callbacks
hold a socket's number in the registry of open sockets, so only
the program's handles and the pending operations keep a socket open
(AR-12). See `docs/net.md`.

A small batch, `shared-1`, moves runtime code that both translators kept
into the crate, so that neither keeps a copy of its own (the owner's rule
of 2026-10-05):
- `IoError<S>`: `IO.Error` generic over the glue's string type, with
  accessors for its fields and the index of its `lean_mk_io_error_*`
  builder;
- `io::StoppingSink`: the sink of `IO.Process.output` that stops when it
  cannot grow;
- `semantics::string::lossy_utf8`: Lean's lossy decoding of the system's
  bytes; and `push_unicode_scalar`, now public;
- `sched::await_task`: `Task.get`'s rule, in both modes;
- `io::env::get_tid`, `io::time::current_time_nanos`,
  `ChildProcess::from_pid`, and the texts of `dbgTraceIfShared` and
  `allocprof`;
- one `sched::thread_create_failed`, and a hook that lets a test harness
  take the runtime's own standard-error lines.

Batch `wait-1` moves three wait protocols into `sched`, each as an object
and as keyed functions for a translator whose values have no room for it
(`docs/sched.md`, "The wait cores"):
- waiting for a computation that another context runs (a thunk, a static,
  a constant): `sched::Gate` (4 bytes) and `WaitList`, and
  `step_keyed`, `wait_running_keyed` and `done_keyed`; the waiters wake in
  order, and a computation that needs itself hangs (LB-08);
- the single-thread `sched::Ref<T>` under Lean 4.35's rule, with threads
  mode's API (and `Ref::empty` in both modes), and `sched::ref_keyed`,
  whose closing store is the one made in the frame that took the
  reference; the taker's own read waits, as natively (review RS4-01);
- the deferred resolution of promises dropped in a free, in both modes:
  `DrainScope`, `defer` and `run_deferred`; the list is moved out before
  it is walked, so a free inside a dependent resolves its own promises
  first, as natively (case `tasks/promise_nested_free_order`).

A wait inside a no-suspend scope (a free) is a Rust panic with the reason;
no Lean program reaches it.

Batch `shared-2` moves three pieces of startup logic that only lean2rr
kept into the crate (audit items 4.2, 4.3, 4.5):
- `io::startup::run_main`: `main` on a thread of its own with Lean's stack
  size, or on the calling thread with `LEAN_MAIN_USE_THREAD=0`, and
  libc++'s abort text when the thread cannot be made (`lean_run_main`);
- the feature `startup-fds`: the crate's own ELF constructor opens the
  startup descriptors (a native quirk, `src/io/startup_fds.rs`), and
  `io::startup::ensure_native_descriptors` at `main`'s start;
- `sched::start_lazy`, `ensure_started`, `sched_started` and `deferring`:
  the scheduler is built at the program's first task, promise, `Std.Sync`
  object, timer, signal watcher, socket or DNS lookup, so a program without
  them builds none (the single-thread scheduler only: threads mode has no
  lazy start, and starts eagerly).

It also makes the crate's ELF constructors use no global allocator
(AR-36): the title's constructor copies the arguments into a block it
maps, and both read `/proc` into stack buffers (`tests/ctor_alloc.rs`).

Batch `shared-3` moves the last code both translators kept, the panic and
exit executor (audit item 3.4), into `io::panic`: a panic's plan carried
out (the effect point, Lean's stderr or the process's, the flush of
stdout, then the abort or the exit), the internal panic (its line built on
the stack, with no allocation), the uncaught error and `IO.Process.exit`.
A translator supplies its streams and its ways out through
`io::panic::PanicGlue`, whose defaults are native's behaviour on the
crate's streams; leanrs keeps its own panic settings through it until it
decides. `docs/panic.md` has native's order, the copies' differences and
the resolution; `tests/io_panic.rs` checks the paths in child processes.

The two projects are finishing a cross-test of their runtimes, then moving
to Lean 4.34.0, then extracting the rest of `semantics`, `io` and `sched` in
that order. See `docs/development.md`, the rules for implementors.

## Rules in short

- The crate root denies `unsafe` code (`#![deny(unsafe_code)]`). A file may
  allow it for itself only with an entry in `UNSAFE.md`, behind a feature:
  a native quirk that no safe API can reproduce (three so far:
  `io::argv_title`, feature `proc-title`, where `setProcessTitle` writes the
  title into the arguments' memory, as libuv does; `io::startup_fds`,
  feature `startup-fds`, the ELF constructor that opens the startup
  descriptors before Rust's runtime starts; and `sched::stack_overflow`,
  feature `stack-overflow`, Lean's stack-overflow report for the
  scheduler's context stacks; their proofs are in
  `docs/native-quirks.md`), or a faster implementation behind the opt-in
  feature `unsafe-fast`, with the same behaviour as its safe twin. Without
  `proc-title`, `startup-fds`, `stack-overflow` and `unsafe-fast` (the
  default build, `io`, `sched`, `threads`, `net`), the crate's own code
  contains no `unsafe`, and the root forbids it.
- Every expected value in the tests comes from a native build with Lean
  4.34.0, on aarch64 Linux with glibc 2.39 (the host both translators run
  on). The ports of glibc's `cbrt` and `cbrtf` give glibc 2.39's aarch64
  results on every target; `atanh` and `atanhf` are glibc's formula over
  the platform's `log1p`. Off aarch64 Linux, the four can differ from that
  platform's native Lean (one algorithm everywhere, by the owner's choice).
- Every bug or disagreement found in either translator's runtime becomes a
  test case in `tests/cases/`.
- Each translator pins this crate by commit and upgrades only after its own
  test suite passes.
- The crate builds offline, with no nightly features, on the Rust
  toolchains both translators use. The default build has no dependencies;
  `io` uses rustix, nix and io-uring, `sched` corosensei, rustix and
  signal-hook, `threads` rustix and signal-hook (its `sched::uv`),
  `stack-overflow` nix, and `net` dns-lookup (pinned exactly;
  its `unsafe` audited in
  `UNSAFE.md`), pinned by `Cargo.lock` and built offline from cargo's local
  registry cache.

Bugs in Lean's own runtime that both translators deliberately do not reproduce, each verified first, are listed in `docs/lean-bugs.md`.

Run `scripts/check.sh` before every commit. It caps its heavy steps at 16G of memory (`LEAN_RUNTIME_MEM`), since it runs on a shared host.
