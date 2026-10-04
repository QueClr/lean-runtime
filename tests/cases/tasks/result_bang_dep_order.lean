-- The order of the dependents of `Promise.result?` when the promise is
-- dropped and `result!` hangs in the walk: Lean walks them newest first,
-- so a dependent made after `result!` is queued (and runs), and one made
-- before it is never reached. (Probe DepOrder2 of the review of sched-2,
-- RS2-03; the reference keeps `p` alive until `ref.set none`.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let _before ← IO.mapTask (fun o => IO.eprintln s!"dep before: {repr o}") r
  let t := p.result!
  IO.eprintln s!"t: {← IO.getTaskState t}"
  let _after ← IO.mapTask (fun o => IO.eprintln s!"dep after: {repr o}") r
  let ref ← IO.mkRef (some p)
  IO.eprintln "dropping"
  ref.set none
  IO.eprintln s!"not reached {← IO.hasFinished t}"
