-- Two workers (`LEAN_NUM_THREADS=2`; hunt HSC-01 of lean-runtime). Task `p`
-- sleeps 50 ms on a worker. Its `sync` dependent `s` runs in `p`'s walk, on
-- `p`'s worker, starts `t` and waits for it (after Lean's panic for a
-- `Task.get` in a `sync` task). A `sync` task's `wait_for` raises no worker
-- limit, so `p`'s worker stays busy, and `t` takes the second worker. At
-- 150 ms `main` queues `q`: both workers are busy, so `q` runs only once `t`
-- has ended. A runtime that runs `t` on `s`'s stack and counts one worker for
-- both runs `q` first.
def main (_args : List String) : IO Unit := do
  let p ← IO.asTask (IO.sleep 50)
  let s ← IO.mapTask (sync := true) (fun _ => do
      let t ← IO.asTask (do IO.sleep 300; IO.println "T done")
      let _ ← IO.wait t
      IO.sleep 100
      IO.println "S done") p
  IO.sleep 150
  let q ← IO.asTask (IO.println "Q ran")
  let _ ← IO.wait q
  let _ ← IO.wait s
