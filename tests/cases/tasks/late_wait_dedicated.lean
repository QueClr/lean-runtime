-- LB-13: after `main` returned, a dedicated task creates a default-priority
-- task and waits for it. Natively the new task never runs and the process
-- hangs; the wait should return (judge's LateWait).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    let t ← IO.asTask (pure 5)
    let v ← IO.wait t
    IO.eprintln s!"dedicated got {repr v.toOption}"
  IO.eprintln "main returns"
