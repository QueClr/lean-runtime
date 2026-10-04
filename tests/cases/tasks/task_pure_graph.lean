-- `Task.pure` (`lean_task_pure`): a finished task holding its value, with
-- no task-manager state (`alloc_task(v)`). Its state is `finished`,
-- `IO.cancel` does nothing, a `sync := true` dependent runs at once on the
-- calling thread, an async one is queued as usual, a bind function may
-- return it (the bind task then finishes with its value, `task_bind_fn1`),
-- and `IO.waitAny` picks it before a pending task. Inside a task too.
-- (Compiled Lean creates the pure tasks `m1`, `b1` and `b4` where they are
-- first used, not where they are written; nothing here depends on it.)

def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let ms := args[1]!.toNat!
  let t := Task.pure n
  IO.println s!"pure: {← IO.getTaskState t}, {t.get}"
  IO.cancel t
  IO.println s!"after cancel: {← IO.getTaskState t}, {← IO.wait t}"
  let m1 := t.map (· + 1)
  let m2 ← IO.mapTask (sync := true) (fun x => do
    IO.println s!"sync dependent of a pure task runs at once: {x}"
    return x * 2) t
  IO.println s!"after the sync dependent: {← IO.getTaskState m2}"
  let b1 := t.bind fun x => Task.pure (x + 10)
  let b2 ← IO.bindTask t fun x => return Task.pure (.ok (x + 20))
  let slow ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    return n + 30
  let b3 ← IO.bindTask slow fun r => return Task.pure (r.map (· + 1))
  let b4 := (Task.spawn fun _ => n + 40).bind fun x => Task.pure (x + 1)
  let any ← IO.waitAny [slow.map (fun r => r.toOption.getD 0), Task.pure (n + 50)]
  IO.println s!"waitAny: {any}, slow finished: {← IO.hasFinished slow}"
  -- `inner`'s value comes from a reference read in the task, so that its
  -- `Task.pure` is made there (compiled Lean hoists a pure computation
  -- that depends on nothing out of the task, into `main`).
  let inner ← IO.asTask do
    let v ← (← IO.mkRef (n + 60)).get
    let p := Task.pure v
    let q ← IO.mapTask (sync := true) (fun x => do
      IO.eprintln s!"inner sync dependent of a pure task: {x}"
      return x + 1) p
    return (← IO.wait q).toOption.getD 0
  IO.println s!"m1 {m1.get}, m2 {repr (← IO.wait m2).toOption}, b1 {b1.get}, b2 {repr (← IO.wait b2).toOption}"
  IO.println s!"b3 {repr (← IO.wait b3).toOption}, b4 {b4.get}, inner {repr (← IO.wait inner).toOption}"
