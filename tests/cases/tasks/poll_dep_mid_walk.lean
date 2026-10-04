-- As `wait_dep_mid_walk`, with `IO.waitAny`, then with `IO.hasFinished`
-- polled: both wait until the walk queues the async dependent.

def main (args : List String) : IO Unit := do
  let ms := args.map String.toNat!
  let b ← IO.asTask (IO.sleep ms[0]!.toUInt32)
  let a ← IO.mapTask (fun _ => IO.eprintln "a ran") b
  let _ ← IO.mapTask (sync := true) (fun _ => do
    IO.sleep ms[2]!.toUInt32
    IO.eprintln "sync dep of b done") b
  IO.sleep ms[1]!.toUInt32
  let _ ← IO.waitAny [a]
  IO.eprintln "main got a"
  let b2 ← IO.asTask (IO.sleep ms[0]!.toUInt32)
  let c ← IO.mapTask (fun _ => IO.eprintln "c ran") b2
  let _ ← IO.mapTask (sync := true) (fun _ => do
    IO.sleep ms[2]!.toUInt32
    IO.eprintln "sync dep of b2 done") b2
  IO.sleep ms[1]!.toUInt32
  while !(← IO.hasFinished c) do
    IO.sleep 5
  IO.eprintln "main saw c finished"
