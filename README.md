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
| `io` (feature `io`): glibc `FILE` buffering, files and handles, directories, environment, clock, `errno` to `IO.Error` | The memory protocol: reference counting, ownership, freeing |
| `sched` (feature `sched`): deferred tasks run as coroutines, yield points, promises, `Std.Sync`, Lean's exit behaviour | Hot paths on the translator's own types (the `Nat` fast path, in-place string and array updates) |
| `net` (feature `net`, with `io` and `sched`): TCP, UDP, DNS and interface addresses (`Std.Internal.UV`, `Std.Net`) on the scheduler's event loop | Its promises and `ByteArray`s (the crate calls back to resolve and to allocate them) |

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
`docs/native-quirks.md`). The next, `quirks-2`, makes the two io_uring
rings among the startup descriptors real rings, made and mapped as libuv
makes them, through the io-uring crate, in place of epoll stand-ins.

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
`Promise.result!`'s permanent block would lose the wakeup (LB-32).

`net` (feature `net`; it turns on `io` and `sched`) has Lean's networking
externs: `Std.Internal.UV.TCP` and `UDP` (libuv 1.48's stream and UDP code
over non-blocking sockets, on the scheduler's event loop), `DNS` (glibc's
`getaddrinfo` and `getnameinfo` through dns-lookup, on two helper threads)
and `Std.Net.interfaceAddresses`. The cases of `tests/cases/net` pass
through the driver; eight native bugs are not reproduced (LB-21 to LB-28).
See `docs/net.md`.

The two projects are finishing a cross-test of their runtimes, then moving
to Lean 4.34.0, then extracting the rest of `semantics`, `io` and `sched` in
that order. See `docs/development.md`, the rules for implementors.

## Rules in short

- The crate root denies `unsafe` code (`#![deny(unsafe_code)]`), and the
  default build contains none. A file may allow it for itself only with an
  entry in `UNSAFE.md`: a native quirk that no safe API can reproduce (one
  so far, `io::argv_title`: `setProcessTitle` writes the title into the
  arguments' memory, as libuv does; its proof is in
  `docs/native-quirks.md`), or a faster implementation behind the opt-in
  feature `unsafe-fast`, with the same behaviour as its safe twin.
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
  signal-hook, and `net` dns-lookup (pinned exactly; its `unsafe` audited in
  `UNSAFE.md`), pinned by `Cargo.lock` and built offline from cargo's local
  registry cache.

Bugs in Lean's own runtime that both translators deliberately do not reproduce, each verified first, are listed in `docs/lean-bugs.md`.

Run `scripts/check.sh` before every commit. It caps its heavy steps at 16G of memory (`LEAN_RUNTIME_MEM`), since it runs on a shared host.
