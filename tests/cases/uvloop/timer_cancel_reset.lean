import Std.Internal.UV.Timer
open Std.Internal.UV

/-! `Timer.cancel` and `Timer.reset`:
- cancel of a running one-shot timer drops its promise (which then never
  resolves while the program holds it: the program resolves it itself
  afterwards, so that its release does not resolve it with `none` first)
  and makes the timer initial again,
  so `next` starts it anew;
- cancel of a running repeating timer drops its promise; it keeps ticking,
  and `next` gives a promise for the next tick;
- reset of a running one-shot timer moves its resolution to `timeout` ms
  after the reset.
The timeout comes from argv: the waits are wide margins around it. -/

def main (args : List String) : IO Unit := do
  let ms := args[0]!.toNat!
  -- one-shot cancel
  let t ← Timer.mk ms.toUInt64 false
  let p ← t.next
  t.cancel
  IO.sleep (2 * ms).toUInt32
  IO.println s!"one-shot: cancelled promise resolved: {← IO.hasFinished p.result?}"
  -- held until here, so only the timer could have resolved it
  p.resolve ()
  let p2 ← t.next
  let r ← IO.wait p2.result?
  IO.println s!"one-shot: next after cancel resolves {repr r}"
  -- repeating cancel
  let u ← Timer.mk ms.toUInt64 true
  let u0 ← u.next
  let _ ← IO.wait u0.result?
  let u1 ← u.next
  u.cancel
  IO.sleep (2 * ms).toUInt32
  IO.println s!"repeating: cancelled promise resolved: {← IO.hasFinished u1.result?}"
  u1.resolve ()
  let u2 ← u.next
  let r ← IO.wait u2.result?
  IO.println s!"repeating: next after cancel resolves {repr r}"
  u.stop
  -- one-shot reset
  let v ← Timer.mk (2 * ms).toUInt64 false
  let t0 ← IO.monoMsNow
  let q ← v.next
  IO.sleep ms.toUInt32
  v.reset
  IO.sleep ms.toUInt32
  IO.println s!"reset: resolved before the moved deadline: {← IO.hasFinished q.result?}"
  let r ← IO.wait q.result?
  let dt := (← IO.monoMsNow) - t0
  IO.println s!"reset: resolves {repr r} after at least {3 * ms} ms: {decide (dt ≥ 3 * ms)}"
