-- A dedicated task blocked in `IO.waitAny [r]`; `r`'s walk runs an older
-- `sync` dependent that sleeps 2 s. An unreferenced task finishing at
-- 200 ms notifies nobody (natively deleted at its end, `m_deleted`); a
-- referenced one finishing at 1 s notifies, and `waitAny` wakes then.
-- (Review of sched-2, RS2-08; probe WaitAnyUnref.) The waiting task
-- resolves `ready` right before its wait, and `main` waits for it, so a
-- late start of the task's thread cannot let `waitAny` see `r` resolved
-- at its first look; each sleep that must end after another ends 800 ms
-- to 1 s later, a wide margin on a loaded host (review AR-44).

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let slow ← IO.mapTask (sync := true) (fun _ => do IO.sleep 2000; IO.eprintln "slow sync dep done") r
  let ready ← IO.Promise.new (α := Unit)
  let w ← IO.asTask (prio := .dedicated) do
    ready.resolve ()
    let v ← IO.waitAny [r]
    IO.eprintln s!"waitAny woke: {repr v}"
  let _ ← IO.wait ready.result?
  IO.sleep 100
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep 200
    IO.eprintln "unreferenced task finishes"
  let o ← IO.asTask (prio := .dedicated) do
    IO.sleep 1000
    IO.eprintln "referenced task finishes"
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait slow
  let _ ← IO.wait o
