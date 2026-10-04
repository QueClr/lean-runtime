-- One worker (`LEAN_NUM_THREADS=1`). Task `t` sleeps 50 ms, then waits for
-- its own `IO.mapTask` dependent, which can never run: it waits forever.
-- Natively that is a `wait_for`, which raises the worker limit by one for
-- a pool task, so `b`, queued at 20 ms while `t` held the only worker,
-- starts on a new worker: "B ran", then "main done", then the exit waits
-- for `t` forever. A runtime that counts `t`'s endless wait as holding the
-- worker never runs `b` (leanrs's review of sched-3, AR-15).

def main (_args : List String) : IO Unit := do
  let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
  let t ← IO.asTask (do
    IO.sleep 50
    match ← r.get with
    | some d =>
      IO.eprintln "t waits for its own dependent"
      let _ ← IO.wait d
    | none => IO.eprintln "no dependent")
  let d ← IO.mapTask (fun _ => pure ()) t
  r.set (some d)
  IO.sleep 20
  let _b ← IO.asTask (IO.eprintln "B ran")
  IO.sleep 300
  IO.eprintln "main done"
