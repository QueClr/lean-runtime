import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A signal that comes while no watcher listens is not delivered to a
watcher started later: SIGCHLD (each child's exit) with a repeating watcher,
then with none (its default action, ignore: the program goes on), then with
a new one-shot watcher, which sees only the next child's exit. The signal
number comes from argv. -/

def run (cmd : String) : IO Unit := do
  let _ ← IO.Process.output { cmd := cmd }

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!
  let w ← Signal.mk (Int32.ofInt num) true
  let p ← w.next
  run "true"
  let r ← IO.wait p.result?
  IO.println s!"first watcher got {repr r}"
  w.stop
  run "true"
  IO.println "a child exited with no watcher"
  let w2 ← Signal.mk (Int32.ofInt num) false
  let q ← w2.next
  IO.sleep 200
  IO.println s!"the new watcher saw the earlier signal: {← IO.hasFinished q.result?}"
  run "true"
  let r ← IO.wait q.result?
  IO.println s!"the new watcher got {repr r}"
