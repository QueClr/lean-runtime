import Std.Internal.UV.Timer
open Std.Internal.UV

/-! LB-33, `stop` of a repeating timer. The timer's period is long (10 s),
and `main` waits for the 0th tick, so no tick comes during the test. Then
`arm` takes a new promise from `next` (made on `main`'s thread), whose
`sync` dependent subscribes again on any value, at most CAP times. `stop`
releases the promise's last reference: the dependent runs inside `stop`.
Correct outcome (Lean master, PR #14793): the timer is finished first, so
each `next` there gives a promise the timer does not hold, which reads
`none` once dropped: the dependent runs CAP times, all inside `stop`.
Native 4.34.0: the dependent runs once and sees a running timer; its `next`
frees the promise a second time and stores a new one, which `stop` then
loses; the double free leaves the process hanging at exit.
args: PERIOD (ms) CAP -/

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
  let period := args[0]!.toNat!
  let cap := args[1]!.toNat!
  let n ← IO.mkRef 0
  let t ← Timer.mk period.toUInt64 true
  let p0 ← t.next
  let r ← IO.wait p0.result?
  say s!"0th tick: {if r.isSome then "some" else "none"}"
  arm t n cap
  say "stop: begin"
  t.stop
  say "stop: end"
  IO.sleep 100
  say s!"dependent runs: {← n.get}"
  say "main: end"
