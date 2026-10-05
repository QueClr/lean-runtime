-- One worker (`LEAN_NUM_THREADS=1`). `main` makes `n` pure tasks, each of
-- which prints its number (`dbgTrace`), then waits for the last one first.
-- Natively the only worker takes the tasks in queue order (first come,
-- first served) and runs each to its end before it takes the next: the
-- lines come in creation order, then `main`'s line. Review AR-25 (from
-- lean2rr's switch to lean-runtime's scheduler): a worker of lean-runtime
-- marked every pure task in front of the awaited one started at once, and
-- the awaited one ran first, on `main`'s stack: `task 3` came first.

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  IO.eprintln "start"
  let ts := (List.range n).map fun i => Task.spawn fun _ => dbgTrace s!"task {i}" fun _ => i
  IO.eprintln s!"values {ts.reverse.map Task.get}"
