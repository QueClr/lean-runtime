-- A dedicated task blocked in `IO.waitAny [r]`; the walk of `r` queues a
-- newer async dependent (100 ms), then runs an older `sync` one (300 ms).
-- `waitAny` sleeps on the task manager's condition variable, which only a
-- finish notifies, not an enqueue: it wakes when the async dependent
-- finishes, before the walk ends. (Review of sched-2, RS2-06; probe
-- WaitAnyEarly.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let slow ← IO.mapTask (sync := true) (fun _ => do IO.sleep 300; IO.eprintln "slow sync dep done") r
  let w ← IO.asTask (prio := .dedicated) do
    let v ← IO.waitAny [r]
    IO.eprintln s!"waitAny woke: {repr v}"
  let a ← IO.mapTask (fun _ => do IO.sleep 100; IO.eprintln "async dep done") r
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait slow
  let _ ← IO.wait a
