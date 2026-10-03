-- With one native worker thread: a `sync := true` dependent runs while Lean
-- walks the finished task's dependents, so a task it creates is queued before
-- the older async dependents, and the walk goes from the newest dependent
-- (src/runtime/object.cpp, handle_finished). From lean2rr's RtTaskSyncDep;
-- leanrs's sched/ adoption gate (eager tasks print another order).

def main (args : List String) : IO Unit := do
  let blockMs := args[0]!.toNat!
  let gapMs := args[1]!.toNat!
  let b ← IO.asTask (do IO.sleep blockMs.toUInt32; IO.println "blocker")
  IO.sleep gapMs.toUInt32
  let _ ← IO.mapTask (fun _ => IO.println "async dep") b
  let _ ← IO.mapTask (sync := true) (fun _ => do
    let _ ← IO.asTask (IO.println "made by sync dep")) b
  let _ ← IO.mapTask (fun _ => IO.println "newest async dep") b
