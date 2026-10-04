import Std.Internal.UV.Signal
open Std.Internal.UV

/-! LB-34, `cancel` of a repeating watcher. The `sync` dependent of its
promise records each value, and subscribes again only in its first run,
which is inside `cancel` (the watcher holds the promise's last reference).
Then `main` calls `next`, sends the signal to the process, waits for its
promise, and prints its value and the dependent's values. Correct outcome
(lean-runtime's order: the state before the release; Lean master's
`cancel` is unchanged): the watcher keeps listening without a promise, so
the dependent's `next` stores a new one, which the watcher keeps; `main`'s
`next` gives that same promise, and the signal resolves it: the
dependent's second run (in the walk of `sync` dependents, before `main`
wakes) records it. Native 4.34.0: the dependent's promise is overwritten
without a release, so `main`'s `next` gives another one, the dependent
runs once only, and the double free leaves the process hanging at exit.
args: SIGNAL (Lean's number; SIGUSR1 is sent) -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

def showV (v : Option Int) : String := match v with | some s => s!"some {s}" | none => "none"

partial def arm (s : Signal) (values : IO.Ref (Array String)) : IO Unit := do
  let p ← s.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      values.modify (·.push (showV v))
      if (← values.get).size == 1 then arm s values) p.result?

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!
  let values ← IO.mkRef (#[] : Array String)
  let s ← Signal.mk (Int32.ofInt num) true
  arm s values
  say "cancel: begin"
  s.cancel
  say "cancel: end"
  let q ← s.next
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #["-USR1", toString pid] }
  let r ← IO.wait q.result?
  say s!"main's next after cancel: {showV r}"
  say s!"dependent values: {← values.get}"
  s.stop
  say "stop: end"
