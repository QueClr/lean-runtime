#!/usr/bin/env bash
# Checks run before every commit: build, test and clippy on the Rust
# toolchains both translators use, in every feature configuration; fmt;
# Miri where the unsafe code is; and the plain-rustc builds one translator
# uses (no cargo), with the same cfg flags its driver passes.
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

capped() {
  if [[ "${LEAN_RUNTIME_NO_CAP:-}" == 1 ]] || ! command -v systemd-run >/dev/null; then
    "$@"
  else
    systemd-run --user --scope --quiet --collect \
      -p MemoryMax="${LEAN_RUNTIME_MEM:-16G}" "$@"
  fi
}

# `io`'s dependencies (Cargo.lock) come from cargo's local registry cache:
# every build here is offline.
if ! cargo +"${TOOLCHAINS[0]}" fetch --offline --locked >/dev/null 2>&1; then
  echo "error: a crate in Cargo.lock is missing from cargo's local registry cache;" \
    "run \`cargo fetch --locked\` once (with network access)" >&2
  exit 1
fi

for tc in "${TOOLCHAINS[@]}"; do
  for f in "${FEATURE_SETS[@]}"; do
    echo "== $tc test features=[${f}]"
    capped cargo +"$tc" test --offline --quiet ${f:+--features "$f"}
    echo "== $tc clippy features=[${f}]"
    cargo +"$tc" clippy --offline --quiet --all-targets ${f:+--features "$f"} -- -D warnings
  done
  # Constant folding of libm calls happens only in optimized builds.
  echo "== $tc release test libm_folding"
  capped cargo +"$tc" test --release --offline --quiet --test libm_folding
  # A driver without cargo builds the dependency-free configurations with
  # plain rustc; `io` needs its dependencies' build scripts, so it is built
  # with cargo, offline and from Cargo.lock.
  for f in "" sched; do
    cfgs=()
    [[ -n $f ]] && cfgs=(--cfg "feature=\"$f\"")
    echo "== $tc plain rustc ${cfgs[*]:-}"
    out=$(mktemp -d)
    rustc +"$tc" --edition 2021 --crate-type rlib --crate-name lean_runtime \
      --out-dir "$out" "${cfgs[@]}" src/lib.rs
    rm -rf "$out"
  done
  echo "== $tc cargo build --offline --locked --features io"
  capped cargo +"$tc" build --offline --locked --quiet --features io
done

last="${TOOLCHAINS[${#TOOLCHAINS[@]}-1]}"
cargo +"$last" fmt --check

# Miri runs where `unsafe` can be: the unsafe-fast configurations. Tests that
# call foreign code or switch stacks are marked #[cfg_attr(miri, ignore)].
if cargo +"$last" miri --version >/dev/null 2>&1; then
  for f in "" "unsafe-fast" "io,sched,unsafe-fast"; do
    echo "== miri features=[${f}]"
    capped cargo +"$last" miri test --offline --quiet ${f:+--features "$f"}
  done
else
  echo "warning: Miri is not installed for $last; skipped" >&2
fi
echo "all checks passed"
