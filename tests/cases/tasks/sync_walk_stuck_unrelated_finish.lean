-- A `sync` dependent of `result?` that never returns (it sleeps in a loop,
-- no misuse panic), so the walk of `r` never ends; an unrelated dedicated
-- task, kept referenced, finishes 400 ms in, and its `notify_all` wakes the
-- task blocked in `IO.wait r`. `main` stays in the walk, so the program
-- hangs. A blocking `sync` continuation is documented misuse; native's
-- outcome is followed (not LB-32). (Review of sched-2, RS2-05; probe
-- StuckWalk.)

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
  let o ← IO.asTask (prio := .dedicated) do
    IO.sleep 400
    IO.eprintln "other task finishes"
  let keep ← IO.mkRef [o]
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln s!"not reached {← IO.hasFinished s} {(← keep.get).length}"
