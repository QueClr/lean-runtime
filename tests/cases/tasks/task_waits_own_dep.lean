-- A task waits for a dependent of itself, which runs only after the task
-- returns: natively a deadlock. `main` returns, and the process never
-- exits, since Lean's exit waits for the task (review round 4 of sched-1).

def main (args : List String) : IO Unit := do
  let ms := args.map String.toNat!
  let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
  let t ← IO.asTask (do
    IO.sleep ms[0]!.toUInt32
    match ← r.get with
    | some x =>
      IO.eprintln "task waits for its own dependent"
      let _ ← IO.wait x
      IO.eprintln "task got it"
    | none => IO.eprintln "no dependent")
  let x ← IO.mapTask (fun _ => IO.eprintln "dependent ran") t
  r.set (some x)
  IO.sleep ms[1]!.toUInt32
  IO.eprintln "main done"
