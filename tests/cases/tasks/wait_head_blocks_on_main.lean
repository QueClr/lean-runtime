-- One worker. `b`, queued before `c`, waits for a promise that `main`
-- resolves only after its `IO.wait c` returns. Natively the worker takes
-- `b`, whose wait raises the worker limit by one, and a new worker runs
-- `c`: the program ends. A runtime that runs the queue's head (`b`) on the
-- waiter's stack hangs there: `main` never gets to resolve the promise
-- (review AR-10 (i), corrected; leanrs's probe).

def main (_args : List String) : IO Unit := do
  let p : IO.Promise Nat ← IO.Promise.new
  let b ← IO.asTask (do
    let v ← IO.wait p.result?
    IO.println s!"b got {v.getD 0}")
  let c ← IO.asTask (IO.println "c ran")
  let _ ← IO.wait c
  IO.println "main resolves"
  p.resolve 7
  let _ ← IO.wait b
  IO.println "main done"
