-- LB-52: with `LEAN_NUM_THREADS=4294967295` (this case's .env), a wait in a
-- pool task raises the pool's limit by one, past 2^32 - 1. Natively the limit
-- is an `unsigned`, so the raise wraps it to 0, and no worker takes a queued
-- task again: `b` never runs, and the process hangs after "main: resolving
-- p". `Task.get`'s documentation says that the raise is there so that the
-- process cannot deadlock by threadpool starvation. Correct: `a` gets 42.
-- The threads-mode bug hunt's repro (HMT-02).

def main : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  -- `b` waits for `p`: it is queued only once `p` is resolved
  let b := p.result?.map (fun o => o.getD 0)
  -- a pool task whose wait for `b` raises the pool's limit
  let a ← IO.asTask (do
    IO.println "a: waiting for b"
    (← IO.getStdout).flush
    return b.get)
  IO.sleep 300
  IO.println "main: resolving p"
  (← IO.getStdout).flush
  p.resolve 42
  match ← IO.wait a with
  | .ok v => IO.println s!"a got {v}"
  | .error e => IO.println s!"error {e}"
