-- Control for LB-13, followed as native: after `main` returned, a dedicated
-- task creates another dedicated task, which gets a thread of its own and
-- runs (judge's LateDedChild).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    let _ ← IO.asTask (prio := .dedicated) (IO.eprintln "late dedicated ran")
    IO.eprintln "dedicated done"
  IO.eprintln "main returns"
