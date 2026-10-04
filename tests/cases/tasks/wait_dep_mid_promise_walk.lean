-- As `wait_dep_mid_walk`, for a promise: the task that resolves it walks
-- the promise's dependents, and the newer `sync` one sleeps there.

def main (args : List String) : IO Unit := do
  let ms := args.map String.toNat!
  let p ← IO.Promise.new (α := Unit)
  let a ← IO.mapTask (fun _ => IO.eprintln "async dep ran") p.result?
  let _ ← IO.mapTask (sync := true) (fun _ => do
    IO.sleep ms[2]!.toUInt32
    IO.eprintln "sync dep done") p.result?
  let _ ← IO.asTask (do
    IO.sleep ms[0]!.toUInt32
    p.resolve ())
  IO.sleep ms[1]!.toUInt32
  let _ ← IO.wait a
  IO.eprintln "main got a"
