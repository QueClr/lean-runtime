-- One worker. `IO.waitAny [a, b]` while `a` waits for a promise that
-- `main` resolves only after `waitAny` returns. Natively the worker takes
-- `a`, whose wait raises the worker limit by one, and a new worker runs
-- `b`: `waitAny` returns `b`'s value and the program ends. A runtime that
-- runs a listed task on the waiter's stack (`a`, the head) hangs there
-- (review AR-10, waitAny; leanrs's probe).

def main (_args : List String) : IO Unit := do
  let p : IO.Promise Nat ← IO.Promise.new
  let a ← IO.asTask (do
    let v ← IO.wait p.result?
    IO.println s!"a got {v.getD 0}"
    pure 1)
  let b ← IO.asTask (do IO.println "b ran"; pure 2)
  let r ← IO.waitAny [a, b]
  IO.println s!"waitAny: {r}"
  p.resolve 7
  let _ ← IO.wait a
  IO.println "main done"
