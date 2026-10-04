-- One worker (`LEAN_NUM_THREADS=1`). Task `s` sleeps 50 ms; its `sync`
-- dependent `d`, run in `s`'s walk on `s`'s worker, waits for its own
-- dependent `e`, which can never run: it waits forever (after Lean's panic
-- for a `Task.get` in a `sync` task). `b` is queued at 20 ms. Natively a
-- `sync` task's `wait_for` raises no worker limit (`in_pool` is false), so
-- the worker stays busy and `b` never runs: "main done", then the exit
-- waits forever. A runtime that frees the worker for such a wait, as for a
-- pool task's own (AR-15), runs `b` (leanrs's review of sched-4, AR-16).

def main (_args : List String) : IO Unit := do
  let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
  let s ← IO.asTask (IO.sleep 50)
  let d ← IO.mapTask (sync := true) (fun _ => do
    match ← r.get with
    | some e =>
      IO.eprintln "d waits for its own dependent"
      let _ ← IO.wait e
    | none => IO.eprintln "no dependent") s
  let e ← IO.mapTask (fun _ => pure ()) d
  r.set (some e)
  IO.sleep 20
  let _b ← IO.asTask (IO.eprintln "B ran")
  IO.sleep 300
  IO.eprintln "main done"
