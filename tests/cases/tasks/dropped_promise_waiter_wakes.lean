-- LB-32: a dedicated task blocks in `IO.wait p.result?`; `t := p.result!`
-- exists; `main` drops `p`. The drop resolves `result?` with `none`, and its
-- walk runs `result!`'s `sync` dependent on `main`: the panic line, then a
-- block forever, as documented. Natively the waiter of `result?` is woken
-- only after the walk, so never: a lost wakeup. The correct outcome: the
-- waiter wakes with `none` (`main` still hangs). (Review of sched-2,
-- RS2-01 (a); probe LostWake.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let t := p.result!
  let _w ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait r
    IO.eprintln s!"waiter woke: {repr v}"
  let ref ← IO.mkRef (some p)
  IO.sleep 200
  IO.eprintln "dropping"
  ref.set none
  IO.eprintln s!"not reached {← IO.hasFinished t}"
