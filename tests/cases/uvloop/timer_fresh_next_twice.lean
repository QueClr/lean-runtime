import Std.Internal.UV.Timer
open Std.Internal.UV

/-! Two externs in a row on a fresh repeating timer of period 1 s: its
first `next` starts it, with the 0th tick due at once. Natively the second
extern takes the loop's lock before the loop thread has woken for that
tick, so a second `next` gives the same promise (both resolve with the 0th
tick: "true, true"), and a `reset` moves the tick to 1 s from then (the
promise is still pending 200 ms later: "false"). Before review HU-03
(fixes-14) the single-thread scheduler's catch-up at the second extern ran
the tick that had come due microseconds before, an order natively
improbable: "true, false" and "true". The argument is the period in ms. -/

def main (args : List String) : IO Unit := do
  let period := args[0]!.toNat!.toUInt64
  let t ← Timer.mk period true
  let a ← t.next
  let b ← t.next
  IO.sleep 200
  IO.println s!"next twice: first finished {← IO.hasFinished a.result?}, second finished {← IO.hasFinished b.result?}"
  t.stop
  let u ← Timer.mk period true
  let c ← u.next
  u.reset
  IO.sleep 200
  IO.println s!"next then reset: first finished {← IO.hasFinished c.result?}"
  u.stop
