-- `IO.monoNanosNow` and `IO.monoMsNow` read CLOCK_MONOTONIC (C++'s
-- `steady_clock`): on Linux its origin is the boot, not the start of the
-- process. The case compares them with the uptime in /proc/uptime (seconds
-- since boot, read in the same program) and prints only whether they agree
-- within two seconds, so the output is deterministic. (CLOCK_MONOTONIC stops
-- while the machine is suspended and /proc/uptime does not: on a machine
-- that has been suspended the two differ.)

def uptimeSeconds : IO Nat := do
  let s ← IO.FS.readFile "/proc/uptime"
  return ((s.splitOn ".")[0]!).toNat!

def near (a b : Nat) : Bool := (if a ≥ b then a - b else b - a) ≤ 2

def main (args : List String) : IO Unit := do
  let up ← uptimeSeconds
  let ns ← IO.monoNanosNow
  let ms ← IO.monoMsNow
  IO.println s!"monoNanosNow is the uptime: {near (ns / 1000000000) up}"
  IO.println s!"monoMsNow is the uptime: {near (ms / 1000) up}"
  IO.println s!"arguments {args.length}"
