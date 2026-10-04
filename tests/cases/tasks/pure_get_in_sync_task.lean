-- `Task.get` and `IO.wait` of a `Task.pure` inside a `sync := true` task:
-- the task has its value, so `lean_task_get` returns it at once, with no
-- "`Task.get` called from a `(sync := true)` task" panic (compare
-- `get_in_sync_task`). The values come from references, so that the pure
-- tasks are made at run time. (Review of sched-2, RS2-03.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r ← IO.mkRef 5
  let pt := Task.pure (← r.get)
  let s ← IO.mapTask (sync := true) (fun o => do
    let v ← IO.wait pt
    let u ← IO.mkRef (v + 1)
    let w := Task.pure (← u.get)
    IO.eprintln s!"sync dependent: {repr o}, {v}, {w.get}") p.result?
  IO.eprintln "resolving"
  p.resolve 1
  let _ ← IO.wait s
  IO.eprintln "done"
