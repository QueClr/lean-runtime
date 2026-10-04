import Std.Internal.UV.Signal
open Std.Internal.UV

/-! LB-34, `stop` of a repeating watcher (no signal is sent). The `sync`
dependent of its promise subscribes again on any value, at most CAP
times. `stop` releases the promise's last reference: the dependent runs
inside `stop`. Correct outcome (Lean master, PR #14793): the watcher is
finished first, so each `next` there gives a promise the watcher does not
hold, which reads `none` once dropped: the dependent runs CAP times, all
inside `stop`, and the program exits. Native 4.34.0: the dependent runs
once and sees a running watcher; its `next` frees the promise a second
time and stores a new one, which `stop` then loses; the double free
leaves the process hanging at exit.
args: SIGNAL (Lean's number) CAP -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

partial def arm (s : Signal) (n : IO.Ref Nat) (cap : Nat) : IO Unit := do
  let p ← s.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      n.modify (· + 1)
      let k ← n.get
      say s!"dependent {k}: value {if v.isSome then "some" else "none"}"
      if k < cap then arm s n cap) p.result?

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!
  let cap := args[1]!.toNat!
  let n ← IO.mkRef 0
  let s ← Signal.mk (Int32.ofInt num) true
  arm s n cap
  say "stop: begin"
  s.stop
  say "stop: end"
  say s!"dependent runs: {← n.get}"
