import Std.Internal.UV.Signal
open Std.Internal.UV

/-! `stop` of a repeating watcher releases its promise (a control for
LB-34, against `Signal.stop`'s docstring note that a pending promise is
leaked and its waiter never woken). No signal is sent. The promise's
`sync` dependent does not subscribe again; `stop` releases the promise's
last reference, so the dependent runs once, with `none`, inside `stop`,
natively and here.
args: SIGNAL (Lean's number) -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!
  let n ← IO.mkRef 0
  let s ← Signal.mk (Int32.ofInt num) true
  let p ← s.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      n.modify (· + 1)
      say s!"dependent {← n.get}: value {if v.isSome then "some" else "none"}") p.result?
  say "stop: begin"
  s.stop
  say "stop: end"
  say s!"dependent runs: {← n.get}"
