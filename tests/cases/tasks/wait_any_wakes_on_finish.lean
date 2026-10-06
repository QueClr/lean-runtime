-- A dedicated task blocked in `IO.waitAny [r]`; the walk of `r` queues a
-- newer async dependent (100 ms), then runs an older `sync` one (1 s).
-- `waitAny` sleeps on the task manager's condition variable, which only a
-- finish notifies, not an enqueue: it wakes when the async dependent
-- finishes, before the walk ends. (Review of sched-2, RS2-06; probe
-- WaitAnyEarly.) The task resolves `ready` right before its wait, and
-- `main` waits for it, so a late start of the task's thread cannot let
-- `waitAny` see `r` resolved at its first look; the `sync` dependent ends
-- about a second after the async one, a wide margin on a loaded host
-- (review AR-44).

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let slow ← IO.mapTask (sync := true) (fun _ => do IO.sleep 1000; IO.eprintln "slow sync dep done") r
  let ready ← IO.Promise.new (α := Unit)
  let w ← IO.asTask (prio := .dedicated) do
    ready.resolve ()
    let v ← IO.waitAny [r]
    IO.eprintln s!"waitAny woke: {repr v}"
  let a ← IO.mapTask (fun _ => do IO.sleep 100; IO.eprintln "async dep done") r
  let _ ← IO.wait ready.result?
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait slow
  let _ ← IO.wait a
