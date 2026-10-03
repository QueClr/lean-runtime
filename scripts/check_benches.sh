#!/usr/bin/env bash
# Builds the micro-benchmarks (benches/rust, benches/native) and checks that each Rust binary and
# its native twin print the same checksum on line 1 for a small N. This is the protocol's "output
# first" check; it times nothing (the kernel_ns lines are discarded).
#
#   scripts/check_benches.sh [N]     (default N = 1000)
set -euo pipefail
cd "$(dirname "$0")/.."
N="${1:-1000}"
if [[ -z "${LEAN_RUNTIME_LOCKED:-}" ]]; then
  export LEAN_RUNTIME_LOCKED=1
  exec flock -s /tmp/leanrs-timing.lock "$0" "$@"
fi
(cd benches/rust && cargo build --release --offline --quiet)
(cd benches/native && lake build -q)
same=0; differ=0
for f in benches/rust/src/bin/*.rs; do
  n=$(basename "$f" .rs)
  r=$(benches/rust/target/release/"$n" "$N" | head -1)
  l=$(LEAN_BACKTRACE=0 benches/native/.lake/build/bin/"$n" "$N" | head -1)
  if [[ "$r" == "$l" ]]; then
    same=$((same + 1))
  else
    differ=$((differ + 1)); echo "differ: $n rust=$r native=$l"
  fi
done
echo "$same benchmarks agree, $differ differ"
[[ $differ == 0 ]]
