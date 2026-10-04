import Std.Internal.UV.Signal
open Std.Internal.UV

/-! `Std.Internal.UV.Signal` with SIGUSR1 sent to the program itself (by
`kill`, a child): a one-shot watcher resolves its promise with the signal's
number and finishes (a second `next` gives the same promise); a repeating one
resolves a new promise for each signal; `cancel` drops the promise. After
`stop` of every watcher, SIGUSR1 has its default action again: the next one
ends the program (status 138), after it flushed its output. A signal number
Lean does not know is refused at `next`. The signal number comes from argv. -/

def kill (sig : String) : IO Unit := do
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #[s!"-{sig}", toString pid] }

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!
  let s ← Signal.mk (Int32.ofInt num) false
  let p ← s.next
  IO.println s!"one-shot pending: {!(← IO.hasFinished p.result?)}"
  kill "USR1"
  let r ← IO.wait p.result?
  IO.println s!"one-shot got {repr r}"
  let p2 ← s.next
  IO.println s!"one-shot second next resolved: {← IO.hasFinished p2.result?}"
  let m ← Signal.mk (Int32.ofInt num) true
  for i in [0:2] do
    let q ← m.next
    kill "USR1"
    let r ← IO.wait q.result?
    IO.println s!"repeating {i}: got {repr r}"
  let q ← m.next
  m.cancel
  kill "USR1"
  IO.sleep 100
  IO.println s!"repeating: cancelled promise resolved: {← IO.hasFinished q.result?}"
  q.resolve 0
  let q2 ← m.next
  kill "USR1"
  let r ← IO.wait q2.result?
  IO.println s!"repeating: after cancel got {repr r}"
  match ← (Signal.mk 99 false >>= (·.next)).toBaseIO with
  | .ok _ => IO.println "signal 99: next succeeded"
  | .error e => IO.println s!"signal 99: {e}"
  m.stop
  s.stop
  IO.println "stopped; the next SIGUSR1 ends the program"
  (← IO.getStdout).flush
  kill "USR1"
  IO.sleep 1000
  IO.println "not reached"
