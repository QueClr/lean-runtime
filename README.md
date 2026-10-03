# lean-runtime

The behaviour of Lean 4.34.0's runtime, as one Rust crate shared by two
translators of compiled Lean programs:

- **lean2rr** ([reussir-lang/lean-to-reussir](https://github.com/reussir-lang/lean-to-reussir)) translates to Reussir;
- **leanrs** translates to safe Rust.

Both must produce programs that behave exactly like Lean's own build:
the same stdout, stderr and exit code. Both therefore reimplement what
Lean's C runtime (`src/runtime/*.cpp`, `lean.h`) does. This crate holds the
part of that work that does not depend on how a translator represents Lean
values.

## What is here, and what is not

| In this crate | In each translator's own glue |
|---|---|
| `semantics`: hashing, float and character formatting, `UIntN`/`IntN` rows, `Nat`/`Int` rules over a big-number trait, string-position algorithms on UTF-8 bytes, array edge rules | The representation of Lean values (`Nat` words, strings, arrays, user types) |
| `io` (feature `io`): glibc `FILE` buffering, files and handles, directories, environment, clock, `errno` to `IO.Error` | The memory protocol: reference counting, ownership, freeing |
| `sched` (feature `sched`): deferred tasks run as coroutines, yield points, promises, `Std.Sync`, Lean's exit behaviour | Hot paths on the translator's own types (the `Nat` fast path, in-place string and array updates) |

Functions take views (`&[u8]`, `&str`) and plain data (`u64`, `f64`), and
return plain data or write into a buffer the caller supplies, so either
translator can wrap them without converting values.

## Status

The first part of `semantics` is in: hashing, `Float`/`Float32` formatting
and conversions, the libm rows, the fixed-width integer rows and the `String`
position functions, checked by the rows in `tests/cases/*/*.rows.toml`
(expected values from native Lean 4.34.0, `scripts/gen_rows.py`), with one
micro-benchmark per public function and its native-Lean twin in `benches/`
(`scripts/gen_benches.py`; not timed yet). The two projects are finishing a
cross-test of their runtimes, then moving to Lean 4.34.0, then extracting
the rest of `semantics`, `io` and `sched` in that order. See
`CONTRIBUTING.md`.

## Rules in short

- The default build contains no `unsafe` code
  (`#![forbid(unsafe_code)]`). The opt-in feature `unsafe-fast` may enable
  faster implementations with the same behaviour (see `UNSAFE.md`).
- Every expected value in the tests comes from a native build with Lean
  4.34.0, on aarch64 Linux with glibc 2.39 (the host both translators run
  on). The ports of glibc's `cbrt`, `cbrtf`, `atanh` and `atanhf` exist only
  on aarch64 Linux; elsewhere their callers fail to compile until a port for
  that platform is added.
- Every bug or disagreement found in either translator's runtime becomes a
  test case in `tests/cases/`.
- Each translator pins this crate by commit and upgrades only after its own
  test suite passes.
- The crate builds offline, with no nightly features, on the Rust
  toolchains both translators use.

Bugs in Lean's own runtime that both translators deliberately do not reproduce, each verified first, are listed in `docs/lean-bugs.md`.

Run `scripts/check.sh` before every commit. It caps its heavy steps at 16G of memory (`LEAN_RUNTIME_MEM`), since it runs on a shared host.
