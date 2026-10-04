-- `sync_walk_stuck_unrelated_finish` without the unrelated task: a `sync`
-- dependent of `r` sleeps in a loop forever, and nothing else ever
-- finishes, so the task waiting for `r` never wakes, natively and here (a
-- blocking `sync` continuation, documented misuse; not LB-32).

partial def spin : IO Unit := do
  IO.sleep 100
  spin

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let s ← IO.mapTask (sync := true) (fun _ => spin) r
  let _w ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait r
    IO.eprintln s!"waiter woke: {repr v}"
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln s!"not reached {← IO.hasFinished s}"
