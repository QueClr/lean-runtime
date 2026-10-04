-- `dbgSleep ms f` sleeps `ms` milliseconds, then returns `f ()`. The value
-- depends on argv, so the sleep happens at run time, between the two clock
-- readings; the case prints whether at least `ms` elapsed, and the merged
-- stream shows that the traces around it come in program order.

@[noinline] def slept (ms : UInt32) (n : Nat) : Nat := dbgSleep ms fun _ => n + 1

def main (args : List String) : IO Unit := do
  let n := args.length
  IO.eprintln "before sleep"
  let t0 ← IO.monoMsNow
  let v := slept 300 n
  IO.println s!"value {v}"
  let t1 ← IO.monoMsNow
  IO.eprintln "after sleep"
  IO.println s!"slept at least 300 ms: {decide (t1 - t0 ≥ 300)}"
