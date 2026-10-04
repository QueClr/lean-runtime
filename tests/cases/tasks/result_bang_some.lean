-- `Promise.result!` on a promise resolved with a value: `IO.Option.getOrBlock!`
-- (`lean_option_get_or_block`) returns it. `result!` maps `result?` with
-- `sync := true`, so on a promise already resolved the function runs at once
-- and the task is `Task.pure` of the value (`lean_task_map_core`); on one
-- resolved later it runs in `resolve`, on the resolving thread, here `main`'s
-- and then a dedicated task's.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let p ← IO.Promise.new (α := Nat)
  p.resolve 7
  let t := p.result!
  IO.println s!"resolved first: {← IO.getTaskState t}, {t.get}"
  let q ← IO.Promise.new (α := String)
  let u := q.result!
  IO.println s!"before resolve: {← IO.getTaskState u}"
  q.resolve "from main"
  IO.println s!"after resolve: {← IO.getTaskState u}, {u.get}"
  let s ← IO.Promise.new (α := Nat)
  let v := s.result!
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep ms.toUInt32
    s.resolve 99
  IO.println s!"resolved by a task: {← IO.wait v}"
