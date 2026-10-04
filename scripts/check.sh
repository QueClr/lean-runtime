#!/usr/bin/env bash
# Checks run before every commit: the site's pages up to date
# (site/build.py --check); build, test and clippy on the Rust toolchains both
# translators use, in every feature configuration; the Rust ports of the
# task, sync, refs, taskio and uvloop cases and of the io cases with tasks,
# over `sched` and `io` (tests/sched-driver); fmt; the plain-rustc
# build of the dependency-free configuration, which one translator builds
# without cargo; `sched`'s offline build from Cargo.lock; that every file
# allowing `unsafe` has its UNSAFE.md entry; and, only with
# LEAN_RUNTIME_MIRI=1, Miri where the unsafe code can be.
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
  exec flock -s /tmp/leanrs-timing.lock "$0" "$@"
fi
TOOLCHAINS=(${LEAN_RUNTIME_TOOLCHAINS:-nightly-2026-08-31 nightly-2026-09-30})
FEATURE_SETS=("" "io,sched" "unsafe-fast" "io,sched,unsafe-fast")

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
  # Constant folding of libm calls happens only in optimized builds.
  echo "== $tc release test libm_folding"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --release --offline --quiet --test libm_folding
  # A driver without cargo builds the dependency-free configuration with
  # plain rustc; `io` needs its dependencies' build scripts and `sched`
  # corosensei, so they are built with cargo, offline and from Cargo.lock.
  echo "== $tc plain rustc"
  out=$(mktemp -d)
  rustc +"$tc" --edition 2021 --crate-type rlib --crate-name lean_runtime \
    --out-dir "$out" src/lib.rs
  rm -rf "$out"
  echo "== $tc cargo build --offline --locked --features io,sched"
  capped cargo +"$tc" build --offline --locked --quiet --features io,sched
  # The cases of tests/cases/{tasks,sync,refs,taskio,uvloop} and the io
  # cases with tasks as Rust programs over `sched` and `io`, with a
  # translator's glue, compared with the cases' outcomes.
  echo "== $tc test sched-driver"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --offline --locked --quiet -p sched-driver
  echo "== $tc release test sched-driver"
  capped "${TEST_TIMEOUT[@]}" cargo +"$tc" test --release --offline --locked --quiet -p sched-driver
  echo "== $tc clippy sched-driver"
  cargo +"$tc" clippy --offline --locked --quiet -p sched-driver --all-targets -- -D warnings
done

last="${TOOLCHAINS[${#TOOLCHAINS[@]}-1]}"
cargo +"$last" fmt --all --check

# Miri runs where `unsafe` can be: the unsafe-fast configurations, and with
# `io` the native quirks' unit tests (UNSAFE.md). It is opt-in
# (LEAN_RUNTIME_MIRI=1): the default build has no `unsafe`, so Miri checks
# little there for a large CPU cost on the shared host (owner, 2026-10-04).
# Run it when an `unsafe` item changes (docs/development.md).
# LEAN_RUNTIME_MIRI_FILTER=NAME limits it to the library's unit tests whose
# name holds NAME, in the configuration with every feature: `argv_title` for the native
# quirk of src/io/argv_title.rs, whose tests run on a block laid out as the
# process's arguments (Miri has no process arguments' memory).
# Tests that call foreign code or switch stacks are marked
# #[cfg_attr(miri, ignore)].
if [[ "${LEAN_RUNTIME_MIRI:-}" != 1 ]]; then
  echo "Miri skipped (set LEAN_RUNTIME_MIRI=1 to run it)"
elif cargo +"$last" miri --version >/dev/null 2>&1; then
  filter="${LEAN_RUNTIME_MIRI_FILTER:-}"
  configs=("" "unsafe-fast" "io,sched,unsafe-fast")
  # the native quirks' tests are in `io`: with a filter, only the
  # configuration with every feature can match
  [[ -n "$filter" ]] && configs=("io,sched,unsafe-fast")
  for f in "${configs[@]}"; do
    echo "== miri features=[${f}]${filter:+ unit tests matching $filter}"
    capped "${TEST_TIMEOUT[@]}" cargo +"$last" miri test --offline --quiet ${f:+--features "$f"} \
      ${filter:+--lib -- "$filter"}
  done
else
  echo "warning: Miri is not installed for $last; skipped" >&2
fi
echo "all checks passed"
