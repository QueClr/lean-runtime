-- The control of `result_bang_dropped`: the `result!` task is dropped before
-- its promise, so the dependent is deleted (`deactivate_task`) and never
-- runs; the promise's drop then resolves `result?` with `none` and nothing
-- panics.

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let t := p.result!
  -- `t`'s last use
  IO.println s!"result! pending: {← IO.getTaskState t}"
  -- `p`'s last use
  IO.println s!"promise resolved: {← p.isResolved}"
  IO.println "no panic"
