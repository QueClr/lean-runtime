-- Control for LB-13, followed as native: after `main` returned, a
-- default-priority task creates another and does not wait for it; its own
-- standard worker takes the new task once it is done (judge's
-- LateStdNoWait).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask do
    IO.sleep ms.toUInt32
    let _ ← IO.asTask (IO.eprintln "late task ran")
    IO.eprintln "task done"
  IO.eprintln "main returns"
