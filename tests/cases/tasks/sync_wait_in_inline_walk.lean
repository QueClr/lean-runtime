-- One worker (`LEAN_NUM_THREADS=1`). Pool task `p` sleeps 50 ms, then waits
-- for `x`, the queue's head, which runs on `p`'s context. `x`'s walk runs
-- its newer `sync` dependent `s`, which waits for `x`'s older async
-- dependent `d`, not reached yet: an endless wait in a `sync` continuation
-- (misuse; Lean's panic for a `Task.get` in a `sync` task). Natively `s`'s
-- `wait_for` raises no worker limit, so `b`, queued behind `x`, never runs:
-- "x ran", the panic line, "main done", then a hang. A runtime that frees
-- the worker for that wait runs `b` (our review of sched-4, RS4-01, with
-- AR-16; probe SyncSelfWait).

def main (_args : List String) : IO Unit := do
  let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
  let _p ← IO.asTask (do
    IO.sleep 50
    match ← r.get with
    | some x => let _ ← IO.wait x
    | none => pure ())
  let x ← IO.asTask (IO.eprintln "x ran")
  let d ← IO.mapTask (fun _ => IO.eprintln "d ran") x
  let _s ← IO.mapTask (sync := true) (fun _ => do
    let _ ← IO.wait d
    IO.eprintln "s done") x
  r.set (some x)
  IO.sleep 20
  let _b ← IO.asTask (IO.eprintln "B ran")
  IO.sleep 300
  IO.eprintln "main done"
