import Std.Internal.UV.Signal
open Std.Internal.UV

/-! SIGCHLD arrives while no SIGCHLD watcher listens (default: ignore).
Then a SIGUSR1 watcher starts, then a SIGCHLD watcher, with no yield in
between. Natively the new SIGCHLD watcher never sees the earlier signal. -/

def run (cmd : String) : IO Unit := do
  let _ ← IO.Process.output { cmd := cmd }

def main : IO Unit := do
  let w ← Signal.mk 17 true
  let p ← w.next
  run "true"
  let r ← IO.wait p.result?
  IO.println s!"first watcher got {repr r}"
  w.stop
  run "true"
  IO.println "a child exited with no SIGCHLD watcher"
  let u ← Signal.mk 10 false
  let pu ← u.next
  let w2 ← Signal.mk 17 false
  let q ← w2.next
  IO.sleep 200
  IO.println s!"the new SIGCHLD watcher saw the earlier signal: {← IO.hasFinished q.result?}"
  q.resolve 0
  pu.resolve 0
  u.stop
  w2.stop
