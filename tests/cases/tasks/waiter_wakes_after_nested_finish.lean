-- Two `sync` dependents of a promise's `result?`: the newer, quick one runs
-- first in the walk and finishes, and its own finish notifies
-- (`resolve_core`'s `notify_all`), so a task blocked in `IO.wait r` wakes
-- then, while the older one still sleeps 300 ms in the walk. Waiters wake at
-- the end of the first walk that ends after their task has its value.
-- (Review of sched-2, RS2-05; probe TwoSync.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let slow ← IO.mapTask (sync := true) (fun o => do IO.sleep 300; IO.eprintln s!"slow sync dep done {repr o}") r
  let quick ← IO.mapTask (sync := true) (fun o => IO.eprintln s!"quick sync dep done {repr o}") r
  let w ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait r
    IO.eprintln s!"waiter woke: {repr v}"
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait slow
  let _ ← IO.wait quick
