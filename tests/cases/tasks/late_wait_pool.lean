-- LB-13: after `main` returned, a default-priority task creates another and
-- waits for it. Natively the new task never runs (the only standard worker
-- is the waiting one, and no new worker starts during shutdown) and the
-- process hangs; the wait should return (judge's LateWaitStd).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask do
    IO.sleep ms.toUInt32
    let t ← IO.asTask (pure 5)
    let v ← IO.wait t
    IO.eprintln s!"task got {repr v.toOption}"
  IO.eprintln "main returns"
