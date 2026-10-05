-- LB-13 (docs/lean-bugs.md) while tasks still enqueue: after `main`
-- returned, a dedicated task enqueues a default-priority task every
-- `stepMs`, while the only worker (`LEAN_NUM_THREADS=1`) runs a busy task
-- for `busyMs`. Natively the tasks enqueued while the busy task runs wait
-- in the queue, and its worker runs them when it is done (a worker exits
-- only once the queue is empty during shutdown); the worker then exits,
-- and each task enqueued after that never runs (`spawn_worker` returns at
-- once during shutdown). They should all run. Every event is `stepMs / 2`
-- or more from the next, and each line is printed before the enqueue it
-- reports, so the correct outcome has no race.

def main (args : List String) : IO Unit := do
  let busyMs := args[0]!.toNat!
  let stepMs := args[1]!.toNat!
  let n := args[2]!.toNat!
  let _ ← IO.asTask do
    IO.sleep busyMs.toUInt32
    IO.println "busy task done"
  let _ ← IO.asTask (prio := .dedicated) do
    for i in [0:n] do
      IO.sleep stepMs.toUInt32
      IO.println s!"dedicated enqueues task {i}"
      let _ ← IO.asTask (IO.println s!"late task {i} ran")
  IO.println "main returns"
