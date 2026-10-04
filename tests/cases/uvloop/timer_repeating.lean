import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A repeating `Std.Internal.UV.Timer`: the first `next` resolves at once
(the 0th multiple of the timeout); each later `next` returns a promise for
the next tick, the same one until it resolves; after `stop`, `next` returns
a promise that never resolves. A repeating timer with timeout 0 ticks once (libuv's
repeat 0 means no repeat). The period and the tick count come from argv. -/

def main (args : List String) : IO Unit := do
  let ms := args[0]!.toNat!
  let n := args[1]!.toNat!
  let t ← Timer.mk ms.toUInt64 true
  let t0 ← IO.monoMsNow
  let p ← t.next
  let r ← IO.wait p.result?
  IO.println s!"first tick {repr r}"
  for i in [0:n] do
    let a ← t.next
    let b ← t.next
    let fa ← IO.hasFinished a.result?
    let _ ← IO.wait b.result?
    IO.println s!"tick {i + 1}: pending before {!fa}, same promise resolved: {← IO.hasFinished a.result?}"
  let dt := (← IO.monoMsNow) - t0
  IO.println s!"{n} periods took at least {n * ms} ms: {decide (dt ≥ n * ms)}"
  t.stop
  let q ← t.next
  IO.sleep (2 * ms).toUInt32
  IO.println s!"after stop, next resolved: {← IO.hasFinished q.result?}"
  q.resolve ()
  let z ← Timer.mk 0 true
  let z0 ← z.next
  let _ ← IO.wait z0.result?
  let z1 ← z.next
  IO.sleep (3 * ms).toUInt32
  IO.println s!"timeout 0: first resolved, second resolved: {← IO.hasFinished z1.result?}"
  z.stop
