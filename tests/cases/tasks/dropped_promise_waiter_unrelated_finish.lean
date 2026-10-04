-- LB-32, `dropped_promise_waiter_wakes` with an unrelated dedicated task,
-- kept referenced, that finishes 1 s in. Natively its finish
-- (`notify_all` on the task manager's one condition variable) is what
-- wakes the waiter of the dropped promise's `result?`, about 800 ms late.
-- The correct outcome: the waiter wakes at the drop, before the other task
-- finishes. (Review of sched-2, RS2-01 (a); probe LostWake3.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let t := p.result!
  let _w ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait r
    IO.eprintln s!"waiter woke: {repr v}"
  let o ← IO.asTask (prio := .dedicated) do
    IO.sleep 1000
    IO.eprintln "other task finishes"
  let keep ← IO.mkRef [o]
  let ref ← IO.mkRef (some p)
  IO.sleep 200
  IO.eprintln "dropping"
  ref.set none
  IO.eprintln s!"not reached {← IO.hasFinished t} {(← keep.get).length}"
