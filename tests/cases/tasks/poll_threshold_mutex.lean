import Std.Sync
open Std

-- `t` locks a `BaseMutex` that `main` holds while it polls `t` with
-- `IO.hasFinished` 3000 times without sleeps, and unlocks only then.
-- Natively `t` blocks on its worker, and the program ends. A runtime that
-- runs the polled task on the poller's stack at its polling threshold
-- hangs there (review AR-9, with a mutex; leanrs's probe).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let m ← BaseMutex.new
  m.lock
  let t ← IO.asTask (do
    m.lock
    IO.println "t has the lock"
    m.unlock)
  let mut k := 0
  for _ in [0:n] do
    if ← IO.hasFinished t then k := k + 1
  IO.println s!"finished while polling: {k}"
  m.unlock
  let _ ← IO.wait t
  IO.println "main done"
