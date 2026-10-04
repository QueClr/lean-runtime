import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot `Std.Internal.UV.Timer`: `next` starts it and returns a
promise that resolves `timeout` ms later; a second `next` returns that
promise; `reset` and `cancel` of a finished timer do nothing; `stop` drops
the promise, and `next` then gives a promise that never resolves. The
timeout comes from argv. -/

def main (args : List String) : IO Unit := do
  let ms := args[0]!.toNat!
  let t ← Timer.mk ms.toUInt64 false
  let t0 ← IO.monoMsNow
  let p ← t.next
  IO.println s!"pending after next: {!(← IO.hasFinished p.result?)}"
  let r ← IO.wait p.result?
  let dt := (← IO.monoMsNow) - t0
  IO.println s!"resolved {repr r}, after at least {ms} ms: {decide (dt ≥ ms)}"
  let p2 ← t.next
  IO.println s!"second next resolved at once: {← IO.hasFinished p2.result?}"
  t.reset
  t.cancel
  let p3 ← t.next
  IO.println s!"after reset and cancel, next resolved: {← IO.hasFinished p3.result?}"
  t.stop
  let p4 ← t.next
  IO.sleep (2 * ms).toUInt32
  IO.println s!"after stop, next resolved: {← IO.hasFinished p4.result?}"
  -- held until here: released earlier, it would resolve with `none`
  p4.resolve ()
