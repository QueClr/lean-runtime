-- As pure_chain_io_dep, through a pure `Task.bind`: the IO task waits for the
-- bind task, which waits for its source and then for the task its function
-- returns (review RS1S-01 of sched-1).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let flag ← IO.mkRef false
  let t := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  let u := t.bind fun v => Task.spawn fun _ => v + 1
  let _ ← IO.mapTask (fun v => do flag.set true; IO.eprintln s!"io dependent saw {v}") u
  while !(← flag.get) do
    IO.sleep 10
  IO.eprintln "main saw the flag"
