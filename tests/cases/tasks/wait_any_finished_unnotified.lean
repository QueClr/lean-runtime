-- `IO.waitAny [t2, u]` while `t2` has finished but its walk of dependents
-- has not notified yet: its `sync` dependent queues a task, then sleeps
-- 200 ms. Natively `waitAny` returns `t2`'s value at the walk's end, and
-- `u`, which waits for a promise `main` resolves afterwards, runs on a
-- worker of its own. A runtime that takes `u` for the only unfinished task
-- of the list (the enqueue wakes `waitAny` before the notification) and
-- runs it on `main`'s stack hangs there (leanrs's review of sched-3, AR-10
-- waitAny).

def main (_args : List String) : IO Unit := do
  let p : IO.Promise Nat ← IO.Promise.new
  let t2 ← IO.asTask (pure (some 2))
  let _d ← IO.mapTask (sync := true) (fun _ => do
    let _ ← IO.asTask (pure 0)
    IO.sleep 200) t2
  let u ← IO.asTask (do
    let v ← IO.wait p.result?
    IO.println s!"u got {v}"
    pure v)
  match ← IO.waitAny [t2, u] with
  | .ok v => IO.println s!"waitAny returned {v}"
  | .error e => IO.println s!"waitAny error {e}"
  p.resolve 7
  let _ ← IO.wait u
  IO.println "done"
