-- One worker. `b` then `t` are queued, and `main` polls `t` with
-- `IO.hasFinished` in a loop without sleeps. Natively the worker runs them
-- first come, first served: `B` before `T`. A runtime that runs the polled
-- task on the poller's stack at its polling threshold prints `T` first
-- (review AR-9, corrected; leanrs's probe: native `B T`, 20 of 20).

def main (_args : List String) : IO Unit := do
  let b ← IO.asTask (IO.println "B")
  let t ← IO.asTask (IO.println "T")
  while !(← IO.hasFinished t) do
    pure ()
  let _ ← IO.wait b
  IO.println "main done"
