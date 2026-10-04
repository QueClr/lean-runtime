-- A dedicated task blocked in `IO.waitAny [r]`; `r`'s walk runs an older
-- `sync` dependent that sleeps 600 ms. An unreferenced task finishing at
-- 200 ms notifies nobody (natively deleted at its end, `m_deleted`); a
-- referenced one finishing at 400 ms notifies, and `waitAny` wakes then.
-- (Review of sched-2, RS2-08; probe WaitAnyUnref.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let slow ← IO.mapTask (sync := true) (fun _ => do IO.sleep 600; IO.eprintln "slow sync dep done") r
  let w ← IO.asTask (prio := .dedicated) do
    let v ← IO.waitAny [r]
    IO.eprintln s!"waitAny woke: {repr v}"
  IO.sleep 100
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep 200
    IO.eprintln "unreferenced task finishes"
  let o ← IO.asTask (prio := .dedicated) do
    IO.sleep 400
    IO.eprintln "referenced task finishes"
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait slow
  let _ ← IO.wait o
