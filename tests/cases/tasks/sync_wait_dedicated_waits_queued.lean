-- One worker (`LEAN_NUM_THREADS=1`; hunt HSC-01 of lean-runtime, the
-- hunter's deadlock variant). Task `p` sleeps 50 ms on the worker. Its
-- `sync` dependent `s` runs in `p`'s walk, on that worker, starts the
-- dedicated task `d` and waits for it (after Lean's panic for a `Task.get`
-- in a `sync` task): a `sync` task's `wait_for` raises no worker limit, so
-- the worker stays busy. `d` queues `q` and waits for it: a dedicated
-- task's `wait_for` raises no limit either, so `q` never gets the worker: a
-- deadlock (misuse: `sync` continuations should not block). A runtime that
-- runs `d` on `s`'s stack and counts no worker for it runs `q` there.
def main (_args : List String) : IO Unit := do
  let p ← IO.asTask (IO.sleep 50)
  let _s ← IO.mapTask (sync := true) (fun _ => do
      let d ← IO.asTask (prio := .dedicated) (do
        let q ← IO.asTask (IO.eprintln "q ran")
        let _ ← IO.wait q
        IO.eprintln "d done")
      let _ ← IO.wait d
      IO.eprintln "s done") p
  IO.sleep 300
  IO.eprintln "main done"
