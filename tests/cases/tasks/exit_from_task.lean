-- `IO.Process.exit` from a task: the process ends at once with that status.
-- C's `exit` flushes the standard streams, so main's buffered line appears;
-- the task manager is not finalized, so the other pending task never runs
-- (leanrs's review of sched-1).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  IO.println "main starts"
  let _ ← IO.asTask (do IO.sleep 5000; IO.eprintln "other task ran")
  let _ ← IO.asTask (do IO.sleep ms.toUInt32; IO.eprintln "task exits"; (IO.Process.exit 3 : IO Unit))
  IO.sleep 1000
  IO.eprintln "not reached"
