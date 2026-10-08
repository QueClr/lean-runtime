-- One worker (`LEAN_NUM_THREADS=1`; hunt HSC-01 of lean-runtime, second
-- shape). Task `p` sleeps 50 ms on the worker. Its `sync` dependent `s`
-- runs in `p`'s walk, on that worker, starts the dedicated task `d` and
-- waits for it (after Lean's panic for a `Task.get` in a `sync` task). A
-- `sync` task's `wait_for` raises no worker limit, so the worker stays busy
-- until `s` ends. At 150 ms `main` queues `q`, which runs only then. A
-- runtime that runs `d` on `s`'s stack and counts no worker for it (a
-- dedicated task has a thread of its own) runs `q` first.
def main (_args : List String) : IO Unit := do
  let p ← IO.asTask (IO.sleep 50)
  let s ← IO.mapTask (sync := true) (fun _ => do
      let d ← IO.asTask (prio := .dedicated) (do IO.sleep 300; IO.println "D done")
      let _ ← IO.wait d
      IO.println "S done") p
  IO.sleep 150
  let q ← IO.asTask (IO.println "Q ran")
  let _ ← IO.wait q
  let _ ← IO.wait s
