import Std.Internal.UV.Timer
open Std.Internal.UV

/-! `stop` of a one-shot timer before it fires (a control for LB-33). The
`sync` dependent of its promise subscribes again (`t.next`) on any value,
at most CAP times. `stop` releases the promise's last reference: the
dependent runs inside `stop`, and each `next` there gives a promise that
reads `none` once dropped, so it runs CAP times, all inside `stop`. That is
native's outcome (4.34.0 gives the promise being released, which has read
`none`), and the correct one (Lean master: a new promise that the stopped
timer does not hold). Review RF2-L-01.
args: TIMEOUT (ms) CAP -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

partial def arm (t : Timer) (n : IO.Ref Nat) (cap : Nat) : IO Unit := do
  let p ← t.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      n.modify (· + 1)
      let k ← n.get
      say s!"dependent {k}: value {if v.isSome then "some" else "none"}"
      if k < cap then arm t n cap) p.result?

def main (args : List String) : IO Unit := do
  let ms := args[0]!.toNat!
  let cap := args[1]!.toNat!
  let n ← IO.mkRef 0
  let t ← Timer.mk ms.toUInt64 false
  arm t n cap
  say "stop: begin"
  t.stop
  say "stop: end"
  IO.sleep 100
  say s!"dependent runs: {← n.get}"
