-- `IO.Promise.result?` (`lean_io_promise_result_opt`) is the promise's own
-- task, the same one at every call. Before `resolve` it is running (a
-- promise's task has no closure: `get_task_state` answers 1), and a task
-- waiting for it blocks; after `resolve` it holds `some v`, and a second
-- `resolve` changes nothing. A promise dropped unresolved resolves it with
-- `none` (`deactivate_promise`), its dependents then see `none`, and
-- `resultD` gives the default.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  IO.println s!"before resolve: {← IO.getTaskState r}, finished {← IO.hasFinished p.result?}, resolved {← p.isResolved}"
  let w ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait p.result?
    IO.eprintln s!"waiter woke: {repr v}"
    return v
  IO.sleep ms.toUInt32
  IO.println s!"waiter finished before resolve: {← IO.hasFinished w}"
  p.resolve 41
  p.resolve 42
  IO.println s!"after resolve: {← IO.getTaskState r}, {repr r.get}, again {repr (← IO.wait p.result?)}, resolved {← p.isResolved}"
  match ← IO.wait w with
  | .ok v => IO.println s!"waiter got {repr v}"
  | .error e => IO.println s!"error {e}"
  let q ← IO.Promise.new (α := Nat)
  let r2 := q.result?
  let m ← IO.mapTask (fun o => IO.eprintln s!"dependent saw {repr o}") r2
  -- `q.resultD 5` below is `q.result?.map (sync := true) (·.getD 5)`, and
  -- compiled Lean shares its `q.result?` with `r2` (common subexpression),
  -- so `q`'s last use is `q.isResolved` here: `q` is dropped right after it,
  -- before the line is printed, which resolves `r2` with `none` and queues
  -- `m`; `resultD`'s map then runs at once.
  IO.println s!"before drop: {← IO.getTaskState r2}, resolved {← q.isResolved}"
  let d := q.resultD 5
  IO.println s!"after drop: {← IO.getTaskState r2}, {repr r2.get}, resultD {d.get}"
  let _ ← IO.wait m
  IO.println "done"
