-- `IO.cancel` of `Promise.result?` before the promise is resolved: the task
-- stays running, and its dependents made meanwhile, `sync` or not, see
-- the cancellation when it is resolved (`handle_finished`). `IO.waitAny`
-- over `[p.result?, Task.pure _]` takes the pure task. `IO.cancel` of a
-- `Task.pure` does nothing (its `m_value` is set), so its dependent is not
-- canceled. (Probe CancelMix of the review of sched-2, RS2-03.)

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  IO.cancel p.result?
  let d ← IO.mapTask (fun _ => IO.checkCanceled) p.result?
  let s ← IO.mapTask (sync := true) (fun _ => IO.checkCanceled) p.result?
  IO.println s!"state after cancel: {← IO.getTaskState p.result?}"
  let a ← IO.waitAny [p.result?, Task.pure (some 3)]
  IO.println s!"waitAny: {repr a}"
  p.resolve 1
  IO.println s!"canceled promise deps: {← IO.wait d} {← IO.wait s}"
  let t := Task.pure 1
  IO.cancel t
  let d2 ← IO.mapTask (fun _ => IO.checkCanceled) t
  IO.println s!"pure dep: {← IO.wait d2}"
