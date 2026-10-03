-- When `main` returns, Lean's task manager sets its shutdown flag, so
-- `IO.checkCanceled` is true in an IO task from then on, and the exit waits
-- for the task (leanrs exit probe 5; `src/runtime/object.cpp`,
-- `~task_manager`).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask do
    IO.sleep ms.toUInt32
    let c ← IO.checkCanceled
    IO.eprintln s!"canceled after main: {c}"
  IO.println "main returns"
