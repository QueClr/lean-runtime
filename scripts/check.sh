#!/usr/bin/env bash
# Checks run before every commit: the site's pages up to date
# (site/build.py --check); build, test and clippy on the Rust toolchains both
# translators use, in every feature configuration; the Rust ports of the
# task, sync, refs, taskio, uvloop and net cases and of the io cases with
# tasks, over `sched`, `io` and `net` (tests/sched-driver), and the ports of
# the task, sync, refs, taskio and net cases, shared with it, in threads
# mode, 5 runs each (tests/sched-driver-mt), both drivers in debug and
# release builds; fmt; the
# plain-rustc build of the dependency-free configuration, which one
# translator builds without cargo; the optional modules' offline build from
# Cargo.lock; that every file allowing `unsafe` has its UNSAFE.md entry; and,
# only with
# LEAN_RUNTIME_MIRI=1, Miri where the unsafe code can be.
#
# The feature configurations: the default build; `io,sched` and `sched,net`
# (`net` needs a scheduler and turns none on), which compile no `unsafe`
# code of the crate (the root forbids it); `threads` (threads mode,
# docs/threads.md, which excludes `sched`), also with every feature that may
# go with it but `net` (`io,threads,proc-title,startup-fds,stack-overflow,
# unsafe-fast`: there the io twins, tests/io_cases.rs and
# tests/io2_cases.rs, run inside tasks, and the uvloop twins,
# tests/threads_twins.rs, over threads mode's `sched::uv`), and with `net`
# (`io,threads,net`, which compiles no `unsafe` code of the crate, and
# `io,threads,net,proc-title,stack-overflow`: `net` on `sched::uv`'s loop
# thread, its unit tests of threads mode, src/net/tests_mt.rs, with the
# loop's own epoll instance and eventfd, as without native's startup
# descriptors; docs/threads.md, 0.7);
# `io,proc-title`, with the native quirk of the process title
# (src/io/argv_title.rs: its unit tests and the twins of the cases that set
# a title), with the test glue's own startup constructor
# (tests/io_cases.rs), and `io` without `sched`; `sched,stack-overflow`,
# with the native quirk of Lean's stack-overflow report
# (src/sched/stack_overflow.rs: its unit tests), and `sched` without `io`;
# `unsafe-fast`; every feature but `net`
# (`io,sched,proc-title,startup-fds,stack-overflow,unsafe-fast`); and
# lean2rr's production set (`io,sched,net,proc-title,startup-fds,
# stack-overflow`, review RSH2-09). The three configurations with
# `startup-fds` have the native quirk of the startup constructor
# (src/io/startup_fds.rs: its unit tests, and the io twins with the crate's
# constructor in place of the test glue's). tests/ctor_alloc.rs
# (AR-36: no global allocator in the crate's constructors) runs wherever
# `io` is on, tests/keyed_alloc.rs (AR-40: no allocation for a keyed claim
# or take with no waiter) wherever `sched` is. Miri runs its own
# configurations (below), which leave `stack-overflow` out: Miri cannot
# model signal delivery. tests/sched-driver builds the crate with
# `stack-overflow` (the twin of `tasks/stack_overflow_in_task`, the `so_*`
# tests); tests/sched-driver-mt with `threads,io,net,stack-overflow`, in a
# cargo invocation of its own (`threads` and `sched` exclude each other).
#
# The host is shared: the heavy steps (cargo test, Miri) run inside a memory
# cap, `systemd-run --user --scope -p MemoryMax=$LEAN_RUNTIME_MEM` (default
# 16G). Set LEAN_RUNTIME_NO_CAP=1 when the caller already provides one.
set -euo pipefail
cd "$(dirname "$0")/.."
# Hold the host's timing lock shared, as every build here does: a timing
# session takes it exclusively and must not overlap with builds.
if [[ -z "${LEAN_RUNTIME_LOCKED:-}" ]]; then
  export LEAN_RUNTIME_LOCKED=1
  # A timing session (flock -x on the gate, then on the lock) holds the gate
  # while it waits for the lock: wait behind it instead of overtaking it,
  # unless this job already holds the lock (LEANRS_TIMING_HELD, set by every
  # shared holder: waiting at the gate then would deadlock with the session).
  [[ ${LEANRS_TIMING_HELD:-} == /tmp/leanrs-timing.lock ]] || flock -s /tmp/leanrs-timing.gate true
  export LEANRS_TIMING_HELD=/tmp/leanrs-timing.lock
  exec flock -s /tmp/leanrs-timing.lock "$0" "$@"
fi
TOOLCHAINS=(${LEAN_RUNTIME_TOOLCHAINS:-nightly-2026-08-31 nightly-2026-09-30})
FEATURE_SETS=("" "io,sched" "sched,net" "io,proc-title" "sched,stack-overflow" "unsafe-fast"
  "io,sched,proc-title,startup-fds,stack-overflow,unsafe-fast" "threads"
  "io,threads,proc-title,startup-fds,stack-overflow,unsafe-fast"
  "io,threads,net" "io,threads,net,proc-title,stack-overflow"
  "io,sched,net,proc-title,startup-fds,stack-overflow")

# Every test run has a deadline (LEAN_RUNTIME_TEST_TIMEOUT seconds, default
# 3600), so a test that blocks fails the check instead of hanging it. It is
# a program (`timeout`), not a shell function, since `capped` may exec its
# command through systemd-run.
TEST_TIMEOUT=(timeout "${LEAN_RUNTIME_TEST_TIMEOUT:-3600}")

capped() {
  if [[ "${LEAN_RUNTIME_NO_CAP:-}" == 1 ]] || ! command -v systemd-run >/dev/null; then
    "$@"
  else
    systemd-run --user --scope --quiet --collect \
      -p MemoryMax="${LEAN_RUNTIME_MEM:-16G}" "$@"
  fi
}

# The dependencies of `io` and `sched` (Cargo.lock) come from cargo's local
# registry cache: every build here is offline.
if ! cargo +"${TOOLCHAINS[0]}" fetch --offline --locked >/dev/null 2>&1; then
  echo "error: a crate in Cargo.lock is missing from cargo's local registry cache;" \
    "run \`cargo fetch --locked\` once (with network access)" >&2
  exit 1
fi

# The site (site/) is built from the repository's files: its pages must be
# up to date, with no broken link and no warning. Cheap, so it runs first.
echo "== site/build.py --check"
python3 site/build.py --check

# `unsafe` in the crate only where UNSAFE.md has an entry: the crate root
# denies `unsafe_code` (`deny` lets a file allow it for itself), and any other
# file that names `unsafe_code` outside a comment (an `allow`, an `expect`, a
# `cfg_attr`, on one line or several) needs an entry headed "### `<path>`".
echo "== unsafe files"
code_lines() { grep -rnP '^(?!\s*//).*\bunsafe_code\b' "$@" || true; }
if ! code_lines src/lib.rs | grep -q 'deny(unsafe_code)' ||
  code_lines src/lib.rs | grep -vqE '(deny|forbid)\(unsafe_code\)'; then
  echo "error: src/lib.rs must deny unsafe_code and allow it nowhere" >&2
  exit 1
fi
while IFS= read -r f; do
  if [[ "$f" != src/lib.rs ]] && ! grep -qF "### \`$f\`" UNSAFE.md; then
    echo "error: $f names unsafe_code, and UNSAFE.md has no entry for it" >&2
    exit 1
  fi
done < <(code_lines src | cut -d: -f1 | sort -u)

for tc in "${TOOLCHAINS[@]}"; do
  for f in "${FEATURE_SETS[@]}"; do
    echo "== $tc test features=[${f}]"
    capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --offline --quiet ${f:+--features "$f"}
    echo "== $tc clippy features=[${f}]"
    cargo +"$tc" clippy --offline --quiet --all-targets ${f:+--features "$f"} -- -D warnings
  done
  # Threads mode with a C-style entry: the alternate signal stacks of ended
  # threads are reused (review RT1-03).
  echo "== $tc example threads_altstack_reuse"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" run --offline --quiet --features threads,stack-overflow \
    --example threads_altstack_reuse
  # Constant folding of libm calls, and the pairing of a sine and a cosine
  # into one sincos call, happen only in optimized builds.
  echo "== $tc release test libm_folding"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --release --offline --quiet --test libm_folding
  # A driver without cargo builds the dependency-free configuration with
  # plain rustc; `io` needs its dependencies' build scripts, `sched`
  # corosensei, and `threads` rustix and signal-hook (its `sched::uv`, T2),
  # so they are built with cargo, offline and from Cargo.lock.
  echo "== $tc plain rustc"
  out=$(mktemp -d)
  rustc +"$tc" --edition 2021 --crate-type rlib --crate-name lean_runtime \
    --out-dir "$out" src/lib.rs
  rm -rf "$out"
  echo "== $tc cargo build --offline --locked --features io,sched,net"
  capped cargo +"$tc" build --offline --locked --quiet --features io,sched,net
  # The cases of tests/cases/{tasks,sync,refs,taskio,uvloop,net} and the io
  # and process cases with tasks as Rust programs over `sched` and `io`, with
  # a translator's glue, compared with the cases' outcomes.
  echo "== $tc test sched-driver"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --offline --locked --quiet -p sched-driver
  echo "== $tc release test sched-driver"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --release --offline --locked --quiet -p sched-driver
  echo "== $tc clippy sched-driver"
  cargo +"$tc" clippy --offline --locked --quiet -p sched-driver --all-targets -- -D warnings
  # The same ports of the task, sync, refs, taskio and net cases in threads
  # mode (`sched::mt`, `net` on `sched::uv`'s loop thread), each case 5
  # times, a few cases at a time (`SCHED_MT_JOBS`, default 6).
  echo "== $tc test sched-driver-mt"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --offline --locked --quiet -p sched-driver-mt
  echo "== $tc release test sched-driver-mt"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --release --offline --locked --quiet -p sched-driver-mt
  echo "== $tc clippy sched-driver-mt"
  cargo +"$tc" clippy --offline --locked --quiet -p sched-driver-mt --all-targets -- -D warnings
done

last="${TOOLCHAINS[${#TOOLCHAINS[@]}-1]}"
cargo +"$last" fmt --all --check

# Miri runs where `unsafe` can be: the unsafe-fast configurations, and with
# `proc-title` the native quirk's unit tests (UNSAFE.md); and on threads
# mode's unit tests (`sched::mt`, features `io,threads`: std's threads, locks
# and condition variables, where Miri finds data races and leaks; the tests
# that need system calls are ignored under Miri). It is opt-in
# (LEAN_RUNTIME_MIRI=1): the default build has no `unsafe`, so Miri checks
# little there for a large CPU cost on the shared host (owner, 2026-10-04).
# Run it when an `unsafe` item changes (docs/development.md).
# LEAN_RUNTIME_MIRI_FILTER=NAME limits it to the library's unit tests whose
# name holds NAME, in the configuration `io,sched,proc-title,unsafe-fast`:
# `argv_title` for the native quirk of src/io/argv_title.rs (feature
# `proc-title`), whose tests run on a block laid out as the process's
# arguments (Miri has no process arguments' memory).
# Tests that call foreign code or switch stacks are marked
# #[cfg_attr(miri, ignore)].
if [[ "${LEAN_RUNTIME_MIRI:-}" != 1 ]]; then
  echo "Miri skipped (set LEAN_RUNTIME_MIRI=1 to run it)"
elif cargo +"$last" miri --version >/dev/null 2>&1; then
  filter="${LEAN_RUNTIME_MIRI_FILTER:-}"
  configs=("" "unsafe-fast" "io,sched,proc-title,unsafe-fast")
  # the native quirk's tests are in `io` with `proc-title`: with a filter,
  # only that configuration can match
  [[ -n "$filter" ]] && configs=("io,sched,proc-title,unsafe-fast")
  for f in "${configs[@]}"; do
    echo "== miri features=[${f}]${filter:+ unit tests matching $filter}"
    capped "${TEST_TIMEOUT[@]}" cargo +"$last" miri test --offline --quiet ${f:+--features "$f"} \
      ${filter:+--lib -- "$filter"}
  done
  if [[ -z "$filter" ]]; then
    # with `io`, so that the tests of the io layer's per-thread state in
    # threads mode run too (review RT2-06)
    echo "== miri features=[io,threads] unit tests of sched::mt"
    capped "${TEST_TIMEOUT[@]}" cargo +"$last" miri test --offline --quiet --features io,threads \
      --lib -- sched::mt
  fi
else
  echo "warning: Miri is not installed for $last; skipped" >&2
fi
echo "all checks passed"
