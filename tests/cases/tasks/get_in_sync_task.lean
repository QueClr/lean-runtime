-- `IO.wait` of an unfinished task inside a `sync := true` dependent: Lean
-- prints "`Task.get` called from a `(sync := true)` task" (object.cpp,
-- `task_manager::wait_for`, a Lean panic, which goes on) and then waits
-- (leanrs's review of sched-1, item 8).

def main (args : List String) : IO Unit := do
  let a := args[0]!.toNat!
  let b := args[1]!.toNat!
  let ta ← IO.asTask (IO.sleep a.toUInt32)
  let tb ← IO.asTask (do IO.sleep b.toUInt32; return 5)
  let d ← IO.mapTask (sync := true) (fun _ => do
    let v ← IO.wait tb
    IO.eprintln s!"dependent got {repr v.toOption}") ta
  let _ ← IO.wait d
  IO.eprintln "main done"
