-- When a task finishes, Lean sets its value, runs its `sync` dependents
-- (`handle_finished`), and only then wakes the threads waiting for it
-- (`resolve_core`: `notify_all` after the walk). So a dedicated task blocked
-- in `IO.wait` on a promise's `result?` wakes after the promise's slow
-- `sync` dependent has returned. The waiter reports what it saw; `main`
-- prints it after waiting, so no line races with `main`'s. (Review of
-- sched-2, RS2-01 (b); the judge's probe SyncOrder.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let s ← IO.mapTask (sync := true) (fun o => do IO.sleep 300; IO.eprintln s!"sync dep done {repr o}") r
  let w ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait r
    return s!"waiter woke: {repr v}, sync dep finished: {← IO.hasFinished s}"
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  match ← IO.wait w with
  | .ok m => IO.eprintln m
  | .error e => IO.eprintln s!"waiter: {e}"
