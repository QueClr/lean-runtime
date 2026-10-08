#!/usr/bin/env bash
# Builds the micro-benchmarks (benches/rust, benches/native) and checks them, timing nothing
# (benches/README.md):
#   1. no generated native loop rebuilds a float literal (Float.ofScientific) or a big-number
#      literal (lean_cstr_to_nat) on each iteration;
#   2. every Rust kernel still contains a loop after optimization (no timed region folded away);
#   3. each Rust binary and its native twin print the same checksum on line 1 for a small N.
#
#   scripts/check_benches.sh [N]     (default N = 1000)
#
# Builds and runs are capped as in scripts/check.sh (LEAN_RUNTIME_MEM, default 16G).
set -euo pipefail
cd "$(dirname "$0")/.."
N="${1:-1000}"
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

capped() {
  if [[ "${LEAN_RUNTIME_NO_CAP:-}" == 1 ]] || ! command -v systemd-run >/dev/null; then
    "$@"
  else
    systemd-run --user --scope --quiet --collect \
      -p MemoryMax="${LEAN_RUNTIME_MEM:-16G}" -p MemorySwapMax=0 "$@"
  fi
}

(cd benches/rust && capped cargo build --release --offline --quiet)
(cd benches/native && capped lake build -q)

python3 - <<'EOF'
import os, re, subprocess, sys, tomllib

failures = []
loops = 0
# 1. the loops of the generated native C
ir = "benches/native/.lake/build/ir/Bench"
for f in sorted(os.listdir(ir)):
    if not f.endswith(".c") or f == "Harness.c":
        continue
    text = open(os.path.join(ir, f)).read()
    for m in re.finditer(r"^(?:LEAN_EXPORT|static) [^\n(]*\b(\w*_(?:loop|walk)\w*)\([^\n]*\)\s*\{",
                         text, re.M):
        # the body: up to the brace that closes the function's
        depth, i = 1, m.end()
        while depth and i < len(text):
            depth += {"{": 1, "}": -1}.get(text[i], 0)
            i += 1
        body = text[m.end():i]
        for bad in ("ofScientific", "lean_cstr_to_nat"):
            if bad in body:
                failures.append(f"{f}: {m.group(1)} calls {bad} in its loop")
        loops += 1

# 2. a loop in every Rust kernel
manifest = tomllib.load(open("benches/benches.toml", "rb"))["bench"]
release = "benches/rust/target/release"
for b in manifest:
    exe = os.path.join(release, b["name"])
    syms = subprocess.run(["nm", "-p", "--defined-only", exe], capture_output=True, text=True).stdout
    dem = subprocess.run(["nm", "-p", "-C", "--defined-only", exe], capture_output=True, text=True).stdout
    mangled = [a.split()[-1] for a, d in zip(syms.splitlines(), dem.splitlines())
               if d.endswith(f" {b['name']}::kernel")]
    if len(mangled) != 1:
        failures.append(f"{b['name']}: no kernel symbol")
        continue
    asm = subprocess.run(["objdump", "-d", "--no-show-raw-insn", f"--disassemble={mangled[0]}", exe],
                         capture_output=True, text=True).stdout
    loop = False
    for line in asm.splitlines():
        ins = re.match(r"\s*([0-9a-f]+):\s+(b(?:\.\w+)?|cbn?z|tbn?z)\s+(?:\w+,\s*)*(?:#\w+,\s*)?([0-9a-f]+)\s", line)
        if ins and int(ins.group(3), 16) <= int(ins.group(1), 16):
            loop = True
            break
    if not loop:
        failures.append(f"{b['name']}: the Rust kernel has no loop")

for f in failures:
    print("FAIL", f)
print(f"{len(manifest)} benchmarks: {loops} generated loop functions and the Rust kernels "
      f"checked, {len(failures)} problems")
sys.exit(1 if failures else 0)
EOF

same=0; differ=0
for f in benches/rust/src/bin/*.rs; do
  n=$(basename "$f" .rs)
  # whole outputs, then line 1 (no `| head`, whose early close would break the second write)
  r=$(benches/rust/target/release/"$n" "$N"); r=${r%%$'\n'*}
  l=$(LEAN_BACKTRACE=0 benches/native/.lake/build/bin/"$n" "$N"); l=${l%%$'\n'*}
  if [[ "$r" == "$l" ]]; then
    same=$((same + 1))
  else
    differ=$((differ + 1)); echo "differ: $n rust=$r native=$l"
  fi
done
echo "$same benchmarks agree, $differ differ"
[[ $differ == 0 ]]
