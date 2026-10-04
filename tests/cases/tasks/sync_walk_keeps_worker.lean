-- One worker (`LEAN_NUM_THREADS=1`). Task `s` sleeps 50 ms; its `sync`
-- dependent `d` runs in `s`'s walk of dependents, on `s`'s worker, and
-- sleeps 100 ms. `b` is queued at 20 ms. Natively the finishing worker
-- stays busy for the whole walk (`handle_finished` runs `d` on it), and a
-- `sync` task's waits raise no worker limit, so `b` runs only after the
-- walk: "D done", then "B ran". A runtime that frees the worker when `s`
-- itself ends runs `b` during `d`'s sleep (leanrs's review of sched-4,
-- AR-16). With 20 workers, `b` runs at once on another one: "B ran" first
-- (the driver's test `sync_walk_keeps_worker_w20`).

def main (_args : List String) : IO Unit := do
  let s ← IO.asTask (IO.sleep 50)
  let _d ← IO.mapTask (sync := true) (fun _ => do
    IO.sleep 100
    IO.eprintln "D done") s
  IO.sleep 20
  let _b ← IO.asTask (IO.eprintln "B ran")
  IO.sleep 300
  IO.eprintln "main done"
