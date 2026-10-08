#!/usr/bin/env bash
# Builds the io micro-benchmarks (benches/io/README.md), both sides, and checks that every pair
# prints the same checksum at a small N. It times nothing: timing follows the measurement protocol
# in docs/development.md and runs only in an owner-approved session. The builds run inside the same
# memory cap as check.sh's heavy steps.
set -euo pipefail
cd "$(dirname "$0")/.."
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
TC=${LEAN_RUNTIME_TOOLCHAINS:-nightly-2026-09-30}
TC=${TC##* }
LEAN=${LEAN_TOOLCHAIN:-$HOME/.elan/toolchains/leanprover--lean4---v4.34.0}
N=${1:-1000}
capped() {
  if [[ "${LEAN_RUNTIME_NO_CAP:-}" == 1 ]] || ! command -v systemd-run >/dev/null; then
    "$@"
  else
    systemd-run --user --scope --quiet --collect -p MemoryMax="${LEAN_RUNTIME_MEM:-16G}" "$@"
  fi
}
(cd benches/io/rust && capped cargo +"$TC" build --offline --release --quiet)
(cd benches/io/native && PATH="$LEAN/bin:$PATH" capped "$LEAN/bin/lake" build -q)
fail=0
for b in io_put_str io_read io_get_line proc_spawn_wait proc_spawn_path proc_spawn_cwd proc_output; do
  r=$(benches/io/rust/target/release/$b "$N" | head -1)
  l=$(benches/io/native/.lake/build/bin/$b "$N" | head -1)
  if [[ "$r" == "$l" ]]; then
    echo "same $b $r"
  else
    echo "DIFFERENT $b: rust $r, native $l"
    fail=1
  fi
done
exit $fail
