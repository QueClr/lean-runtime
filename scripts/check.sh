#!/usr/bin/env bash
# Checks run before every commit: build, test, clippy and fmt on the Rust
# toolchains both translators use, in the default configuration and with
# every feature; Miri on the toolchain that has it; and a plain-rustc
# build, as one translator builds the crate.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLCHAINS=(${LEAN_RUNTIME_TOOLCHAINS:-nightly-2026-08-31 nightly-2026-09-30})
FEATURES=("" "io,sched" "io,sched,unsafe-fast")
for tc in "${TOOLCHAINS[@]}"; do
  for f in "${FEATURES[@]}"; do
    echo "== $tc features=[${f}]"
    cargo +"$tc" test --offline --quiet ${f:+--features "$f"}
  done
  cargo +"$tc" clippy --offline --quiet --all-features -- -D warnings
  out=$(mktemp -d)
  rustc +"$tc" --edition 2021 --crate-type rlib --crate-name lean_runtime \
    --out-dir "$out" src/lib.rs
  rm -rf "$out"
done
cargo +"${TOOLCHAINS[-1]}" fmt --check
if cargo +"${TOOLCHAINS[-1]}" miri --version >/dev/null 2>&1; then
  cargo +"${TOOLCHAINS[-1]}" miri test --offline --quiet
fi
echo "all checks passed"
