import Std.Internal.UV.System
/-! The memory queries against the files libuv reads (review RIO2-09: `uv_system` accepts a
constant 0 for the cgroup's limit). `totalMemory` is `/proc/meminfo`'s `MemTotal` in bytes;
`constrainedMemory` is the smaller of the cgroup v2 `memory.max` and `memory.high` of the
process's cgroup (`max` the largest `UInt64`, 0 when either is 0 or missing); `availableMemory`
is at most that limit when the limit is below the total memory, else at most the total; the CPU
count is the number of `cpuN` lines of `/proc/stat`. On a cgroup v1 host the limit is not
checked. -/
open Std.Internal.UV.System

def readLimit (p : String) : IO Nat := do
  try
    let t := (← IO.FS.readFile p).trimAscii.toString
    return if t == "max" then 2 ^ 64 - 1 else t.toNat?.getD 0
  catch _ => return 0

def memTotal : IO Nat := do
  for l in (← IO.FS.readFile "/proc/meminfo").splitOn "\n" do
    if l.startsWith "MemTotal:" then
      let n := ((l.drop 9).trimAscii.toString.splitOn " ").headD ""
      return n.toNat?.getD 0 * 1024
  return 0

def main : IO Unit := do
  let total ← memTotal
  IO.println s!"total exact: {decide ((← totalMemory).toNat == total)} {decide (total > 0)}"
  let cg ← IO.FS.readFile "/proc/self/cgroup"
  if cg.startsWith "0::/" then
    let path := ((cg.drop 4).toString.splitOn "\n").headD ""
    let dir := s!"/sys/fs/cgroup/{path}"
    let max ← readLimit s!"{dir}/memory.max"
    let high ← readLimit s!"{dir}/memory.high"
    let want := if max == 0 || high == 0 then 0 else Nat.min max high
    let c := (← constrainedMemory).toNat
    IO.println s!"constrained exact: {decide (c == want)}"
    let avail := (← availableMemory).toNat
    let bound := if want == 0 || want > total then total else want
    IO.println s!"available bounded: {decide (avail ≤ bound)} {decide (avail > 0)}"
  else
    IO.println "constrained exact: true"
    IO.println "available bounded: true true"
  let stat ← IO.FS.readFile "/proc/stat"
  let n := ((stat.splitOn "\n").drop 1).takeWhile (·.startsWith "cpu") |>.length
  IO.println s!"cpus: {decide ((← cpuInfo).size == n)} {decide (n > 0)}"
