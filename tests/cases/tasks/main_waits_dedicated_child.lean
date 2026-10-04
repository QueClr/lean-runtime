-- Control for LB-13, followed as native: LateWait, but `main` waits for the
-- dedicated task before it returns, so the standard worker the wait needs
-- is started before shutdown and the program finishes (judge's MainWaits).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let d ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    let t ← IO.asTask (pure 5)
    let v ← IO.wait t
    IO.eprintln s!"dedicated got {repr v.toOption}"
  let _ ← IO.wait d
  IO.eprintln "main returns"
