-- A task calls `IO.waitAny` on a list of two tasks: a dependent of itself,
-- which cannot run while the task runs, and a task that finishes. Natively
-- `IO.waitAny` returns once the second one finishes; the dependent runs
-- after the task returns (review RS1S-17 of sched-1). `main` waits for the
-- dependent: had it waited for the task, its last line would race the
-- dependent's (1 run in 12 natively).

def main (args : List String) : IO Unit := do
  let ms := args.map String.toNat!
  let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
  let t ← IO.asTask (do
    IO.sleep ms[0]!.toUInt32
    let some x ← r.get | IO.eprintln "no dependent"
    let y ← IO.asTask (IO.sleep ms[1]!.toUInt32)
    let _ ← IO.waitAny [x, y]
    IO.eprintln "task's waitAny returned")
  let x ← IO.mapTask (fun _ => IO.eprintln "dependent ran") t
  r.set (some x)
  let _ ← IO.wait x
  IO.eprintln "main done"
