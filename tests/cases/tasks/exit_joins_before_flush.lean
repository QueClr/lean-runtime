-- At exit Lean joins the pending tasks before it flushes the standard
-- streams. With stdout and stderr merged into one pipe, the task's unbuffered
-- stderr line therefore comes before main's buffered stdout line, although
-- main printed first (leanrs exit probes, refinement A).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask do
    IO.sleep ms.toUInt32
    IO.eprintln "task done"
  IO.println "main done"
