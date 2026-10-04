-- LB-13: a default-priority `IO.mapTask` of a dedicated task that finishes
-- after `main` returned. Both are created before `main` returns; natively
-- the dependent never runs, since no standard worker is left when it is
-- enqueued. It should run, after the dedicated task's line (judge's
-- LateMapDep).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let d ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    IO.eprintln "dedicated done"
  let _ ← IO.mapTask (fun _ => IO.eprintln "dependent ran") d
  IO.eprintln "main returns"
