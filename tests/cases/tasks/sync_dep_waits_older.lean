-- A `sync` dependent of `b`, run in the walk of `b`'s dependents, waits for
-- an older async dependent of `b`. The walk goes from the newest, so it
-- would queue that one only after the `sync` one returns: natively a
-- deadlock (after the `GET_IN_SYNC_TASK` Lean panic), and the process never
-- exits (review round 4 of sched-1).

def main (args : List String) : IO Unit := do
  let ms := args.map String.toNat!
  let b ← IO.asTask (IO.sleep ms[0]!.toUInt32)
  let a ← IO.mapTask (fun _ => IO.eprintln "async dep ran") b
  let _ ← IO.mapTask (sync := true) (fun _ => do
    IO.eprintln "sync dep waits for a"
    let _ ← IO.wait a
    IO.eprintln "sync dep got a") b
  IO.sleep ms[1]!.toUInt32
  IO.eprintln "main done"
