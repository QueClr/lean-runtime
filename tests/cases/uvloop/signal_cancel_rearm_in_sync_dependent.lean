import Std.Internal.UV.Signal
open Std.Internal.UV

/-! LB-34, `cancel` of a repeating watcher. The `sync` dependent of its
promise records each value, and subscribes again only in its first run,
which is inside `cancel` (the watcher holds the promise's last reference);
its second run resolves `done`. Then `main` calls `next`, sends the signal
to the process, waits for its promise and prints its value, then waits for
`done` and prints the dependent's values. The wait for `done` is needed: a
wait for a promise can return before the walk of its `sync` dependents has
run them, natively too (review AR-41). Correct outcome (lean-runtime's
order: the state before the release; Lean master's `cancel` is
unchanged): the watcher keeps listening without a promise, so the
dependent's `next` stores a new one, which the watcher keeps; `main`'s
`next` gives that same promise, and the signal resolves it: the
dependent's second run records it and resolves `done`. Native 4.34.0: the
dependent's promise is overwritten without a release, so `main`'s `next`
gives another one, the dependent runs once only, and `done` never
resolves: `main` waits forever (a hang).
args: SIGNAL (Lean's number; SIGUSR1 is sent) -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

def showV (v : Option Int) : String := match v with | some s => s!"some {s}" | none => "none"

partial def arm (s : Signal) (values : IO.Ref (Array String)) (done : IO.Promise Unit) : IO Unit := do
  let p ← s.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      values.modify (·.push (showV v))
      if (← values.get).size == 1 then arm s values done else done.resolve ()) p.result?

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!
  let values ← IO.mkRef (#[] : Array String)
  let done ← IO.Promise.new
  let s ← Signal.mk (Int32.ofInt num) true
  arm s values done
  say "cancel: begin"
  s.cancel
  say "cancel: end"
  let q ← s.next
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #["-USR1", toString pid] }
  let r ← IO.wait q.result?
  say s!"main's next after cancel: {showV r}"
  let _ ← IO.wait done.result?
  say s!"dependent values: {← values.get}"
  s.stop
  say "stop: end"
