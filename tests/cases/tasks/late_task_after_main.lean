-- LB-13 (docs/lean-bugs.md): after `main` returned, a dedicated task enqueues
-- a default-priority task once no standard worker is left. Natively it
-- never runs (`spawn_worker` returns at once during shutdown), so its line
-- never appears; it should run. The dedicated task prints before it
-- enqueues, so the correct outcome has no race. (The judge's LateWrite
-- checks the same through a file; `scripts/cases.py` checks no files yet.)

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    IO.println "dedicated enqueues the late task"
    let _ ← IO.asTask (IO.println "late task ran")
  IO.eprintln "main returns"
