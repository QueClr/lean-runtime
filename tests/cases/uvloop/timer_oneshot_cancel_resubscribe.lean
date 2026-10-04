import Std.Internal.UV.Timer
open Std.Internal.UV

/-! LB-33, `cancel` of a one-shot timer before it fires. The `sync`
dependent of its promise subscribes again (`t.next`) on any value, at most
CAP times. `cancel` releases the promise's last reference: the dependent
runs inside `cancel`. Correct outcome (lean-runtime's order: the state
before the release; Lean master's `cancel` is unchanged): the timer is
initial again, so the dependent's `next` starts it again, and its promise
stays pending; `stop` then drops that promise, and inside `stop` the
dependent runs again with `none` up to CAP (a stopped timer's `next` gives
a promise the timer does not hold). Native 4.34.0: inside `cancel`, `next`
gives the promise being released, which has read `none`, so the dependent
runs CAP times there.
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
  say "cancel: begin"
  t.cancel
  say "cancel: end"
  IO.sleep 100
  say s!"dependent runs: {← n.get}"
  say "stop: begin"
  t.stop
  say "stop: end"
