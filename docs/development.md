# Development

The rules for implementors of lean-runtime, people and agents alike.
`README.md` says what the crate is; the module docs in `src/` and the
files in `docs/` say how each part works.

## Sequence

1. Cross-test the two runtimes, and fix each disagreement in whichever
   runtime is wrong.
2. Move both translators to Lean 4.34.0.
3. Extract `semantics`, then `io`, then `sched`. For each module, keep the
   implementation the cross-tests show correct, and the safe one where both
   are. Both translators' test suites gate every step.

Until a module is extracted, new ports of Lean runtime functions are written
here, not in either translator, unless one of them needs the function at
once.

## Code

- **No `unsafe` without a feature that needs it.**
  Anything that needs `unsafe` comes from a vetted external crate: `nix` or
  `rustix` for system calls, io-uring for the startup rings, corosensei for
  task switching, signal-hook (its safe API only) for signal handlers,
  dns-lookup (feature `net`) for glibc's `getaddrinfo` and `getnameinfo`.
  What
  no crate offers safely stays in each translator's glue, behind a
  contract the crate states and upholds: so far `sched::Glue::suspend`
  (one dereference of a coroutine's yielder; `docs/sched.md`), or is a
  native quirk of the crate (below). The crate root denies
  `unsafe_code` (`#![deny(unsafe_code)]`), and forbids it in a build with
  none of `proc-title`, `stack-overflow` and `unsafe-fast`: the default
  build, `io`, `sched` and `net` compile no `unsafe` code of the crate.
- **Native quirks.** A native behaviour that no safe API can reproduce, and
  that each glue would otherwise write on its own, may be written with
  `unsafe` in the crate (owner, 2026-10-04), behind a feature of its own,
  so that a translator that does not need it compiles no `unsafe` (owner:
  avoid `unsafe` where it is not needed): one small file with
  `#![allow(unsafe_code)]` and
  `#![deny(unsafe_op_in_unsafe_fn)]`, a `// SAFETY:` comment on every
  `unsafe` block, an entry in `UNSAFE.md` and its proof in
  `docs/native-quirks.md`; leanrs reviews it before merge. Try the safe
  routes first, and name them in the entry. Run Miri on its unit tests
  where it can model them (`LEAN_RUNTIME_MIRI=1 scripts/check.sh`).
  `scripts/check.sh` fails on a file that names `unsafe_code` without an
  entry, and runs the quirk's feature in a configuration of its own. So
  far: `src/io/argv_title.rs`, feature `proc-title` (which turns on `io`).
  lean2rr enables it; a translator that leaves it off gets `ENOBUFS` from
  `setProcessTitle` (`getProcessTitle` still gives `argv[0]`;
  `docs/native-quirks.md`, "Without the feature"). And
  `src/sched/stack_overflow.rs`, feature `stack-overflow` (which turns on
  `sched`; AR-11): Lean's stack-overflow report for the scheduler's
  contexts. lean2rr enables it; without it, a task that overflows its
  context's stack ends with a plain SIGSEGV (status 139).
- **`unsafe-fast`.** Every `unsafe` item has its own
  `#[allow(unsafe_code)]` (the crate root denies it). An implementation
  behind this feature must:
  - have the same observable behaviour as the safe one, which stays;
  - have an entry in `UNSAFE.md` with a written proof;
  - run under Miri (`LEAN_RUNTIME_MIRI=1 scripts/check.sh`; Miri is opt-in,
    so set the variable whenever such an item is added or changed), and
    under Kani where that applies (Kani joins the checks with the first
    entry).

  The tests run both configurations.
- **Signatures on views and plain data.** No function here owns, allocates
  or frees a Lean value of either translator.
- **IO without bulk copies.** An io function writes its result into the
  caller's storage (a `&mut [u8]` the caller allocated, or a `ByteSink` it
  implements on its own object) and takes views (`&[u8]`) for data and
  paths, so neither translator's glue copies bulk data (`src/io/mod.rs`).
- **Comments.** Each function names the Lean C function or Lean definition
  it mirrors.
- **Builds.** Offline and with no nightly features. The default build has
  no dependencies, and a plain `rustc --crate-type rlib` build of it must
  work (one translator does not use cargo for it). `io` depends on rustix
  and nix, whose build scripts plain rustc cannot run, and `sched` on
  corosensei, rustix and signal-hook, and `net` on dns-lookup, so they are
  built with cargo: `cargo build --offline --locked --features io,sched,net`
  works from a clean checkout, the versions pinned by
  the committed `Cargo.lock` and the crates taken from cargo's local
  registry cache (`cargo fetch --locked` fills it once; `scripts/check.sh`
  says so when a crate is missing). Nothing is vendored.
- **Dependencies.** Requirements are carets compatible with leanrs's
  offline registry, which leanrs resolves in its own workspace (it ignores
  `Cargo.lock`), with only the features the crate uses. Before adding a
  dependency, or a feature that pulls in more crates, ask leanrs to check it
  against their registry; a new crate that wraps `unsafe` needs the owner's
  decision (nix, rustix, corosensei and signal-hook are approved;
  io-uring 0.7.13, pinned exactly, for the startup rings, was accepted by
  leanrs's shared-runtime coordinator under the owner's delegation of
  dependency decisions (2026-10-04); dns-lookup 2.1.1, pinned exactly, for
  `net` only, approved the same way). The `unsafe` of every new crate or
  feature is audited in `UNSAFE.md`, "Dependencies".

## Tests

- `tests/cases/` holds one case per behaviour, with its expected outcome
  from a native Lean 4.34.0 build; `tests/cases/README.md` has the format
  and the rules (inputs from argv or stdin, deterministic output, expected
  values from compiled Lean, never `#eval`).
- **Every bug or disagreement** either translator finds in its runtime
  becomes a case here, if it concerns runtime behaviour, even when the fix
  lands in a translator.
- **Lean bugs.** Where native Lean is wrong (a confirmed `LB-nn` of
  `docs/lean-bugs.md`), the case expects the correct outcome and records
  native's in its `native` field; `scripts/cases.py expect` re-checks
  native against it.
- **Twins.** A program case that reaches a crate function is also checked
  on the crate: its Rust twin makes the same calls through the crate, with
  what a translator's glue adds. `tests/io_cases.rs` and
  `tests/io2_cases.rs` run their twins through `scripts/cases.py check`;
  `tests/sched-driver` (the `tasks/`, `sync/` and `refs/` cases) runs its
  own the same way, and refuses a case with a field it does not implement.
  A case has no twin only when it tests what a translator generates or
  Lean code a translator compiles, or when a Rust twin cannot start as the
  case requires; its `.toml` then says why, in a comment starting
  `# No twin:` (now the `folding/` cases, `cse/panic_once_across_types`,
  `io/borrow_with_ref_struct`, `time/time_format` and
  `process/closed_stdout`).
- Unit tests next to the code use values from the same cases.
- Run `scripts/check.sh` before every commit. On the shared host, run
  targeted tests while working (the cases touched, their twins' test
  targets, the unit tests of the module) and the full check once at the
  end.

## Performance

- **The floor (O12).** The default build of every function must be at least
  as fast as Lean 4.34.0's native runtime for the same operation;
  `unsafe-fast` only goes further. A slower safe implementation is not
  merged as the default.
- **Benchmarks.** `benches/` holds one micro-benchmark per public function,
  and next to it a native Lean program timing the same operation through
  Lean's normal API; the Rust side does the glue a translator's code does
  (boxed `Nat`/`Int` words, new string objects), so the pair does the same
  work (`benches/README.md`). Only N, the number of iterations, comes from
  argv; the operands come from a fixed-seed LCG and fixed strings, built
  before the timed region and passed through an opaque sink (`black_box`,
  `Bench.pin`), so neither compiler can fold them.
- **Measurement protocol** (shared with leanrs):
  - **The timing lock.** A session holds an exclusive `flock` on
    `/tmp/leanrs-timing.lock`, taken without waiting. It refuses to start if
    the 1-minute load average is above 1.0 or any lake, cargo, rustc, clang
    or lean process is running. Every build and test script holds the same
    lock shared (`scripts/check.sh` does).
  - **Pinning.** One quiet core per session, set with `sched_setaffinity`;
    one timed process at a time; one warm-up per side, then nine alternating
    pairs, each a fresh process.
  - **What is timed.** Each binary reports its own kernel time: monotonic
    clock reads around the call, input built before and result consumed
    after, through opaque sinks. The statistic is the median of nine. A case
    whose native median is under 20 ms is `too-small` and gets a larger
    size.
  - **Spread.** (max - min) / median above 0.15 means a rerun as nineteen
    pairs. A micro still noisy after that is not used.
  - **Memory.** `ru_maxrss` of both sides, reported beside the time ratio.
- **Approval.** Timing sessions run only with the owner's approval, and
  never at the same time as another project's.

## Commits

- Small commits with a plain description of the change and its reason.
- Each translator pins this crate by commit, and moves its pin only after
  its own suite passes.
