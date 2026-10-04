# Contributing

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

- **No `unsafe` in the default build.** Anything that needs `unsafe` comes
  from a vetted external crate: `nix` or `rustix` for system calls,
  corosensei for task switching. What no crate offers safely stays in each
  translator's glue, behind a contract the crate states and upholds: so far
  `sched::Glue::suspend` (one dereference of a coroutine's yielder) and the
  SIGSEGV handler of Lean's stack-overflow report (`docs/sched.md`).
- **`unsafe-fast`.** Every `unsafe` item has its own `#[allow(unsafe_code)]` (the crate root denies it under this feature). An implementation behind this feature must:
  - have the same observable behaviour as the safe one, which stays;
  - have an entry in `UNSAFE.md` with a written proof;
  - run under Miri (`scripts/check.sh` runs Miri on the `unsafe-fast` configurations), and under Kani where that applies (Kani joins the checks with the first entry).

  The tests run both configurations.
- **Signatures on views and plain data.** No function here owns, allocates
  or frees a Lean value of either translator.
- **Builds.** Offline and with no nightly features. The default build has
  no dependencies, and a plain `rustc --crate-type rlib` build of it must
  work (one translator does not use cargo for it). `io` depends on rustix and
  nix, whose build scripts plain rustc cannot run, and `sched` on
  corosensei, so they are built with cargo: `cargo build --offline --locked
  --features io,sched` works from a clean checkout, the versions pinned by
  the committed `Cargo.lock` and the crates taken from cargo's local registry
  cache (`cargo fetch --locked` fills it once; `scripts/check.sh` says so
  when a crate is missing). Nothing is vendored in the repository.
- **Dependencies.** Requirements are carets compatible with leanrs's offline
  registry, which leanrs resolves in its own workspace (it ignores
  `Cargo.lock`), with only the features the crate uses. Before adding a
  dependency, or a feature that pulls in more crates, ask leanrs to check it
  against their registry; a new crate that wraps `unsafe` needs the owner's
  decision (nix, rustix and corosensei are approved).
- **IO without bulk copies.** An io function writes its result into the
  caller's storage (a `&mut [u8]` the caller allocated, or a `ByteSink` it
  implements on its own object) and takes views (`&[u8]`) for data and paths,
  so neither translator's glue copies bulk data (`src/io/mod.rs`).
- **Comments.** Each function names the Lean C function or Lean definition
  it mirrors.

## Tests

- `tests/cases/` holds one case per behaviour, with its expected output
  from a native Lean 4.34.0 build (format in `tests/cases/README.md`).
- **Every bug or disagreement** either translator finds in its runtime
  becomes a case here, if it concerns runtime behaviour, even when the fix
  lands in a translator.
- Unit tests next to the code use values from the same cases.

## Performance

- **The floor.** The default build of every function must be at least as
  fast as Lean 4.34.0's native runtime for the same operation; `unsafe-fast`
  only goes further. A slower safe implementation is not merged as the
  default.
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

Small commits with a plain description of the change and its reason.
Each translator moves its pinned commit after its own suite passes.
