-- An IO task depends on a pure task through a pure `Task.map`, and `main`
-- polls a flag the IO task sets, with sleeps: the pure tasks must run for the
-- IO task, although no IO task waits on them directly (review RS1S-01 of
-- sched-1; probes/PureChainIoDep.lean).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let flag ← IO.mkRef false
  let t := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  let u := t.map (· + 1)
  let _ ← IO.mapTask (fun v => do flag.set true; IO.eprintln s!"io dependent saw {v}") u
  while !(← flag.get) do
    IO.sleep 10
  IO.eprintln "main saw the flag"
