-- `t` waits for a promise that only `main` resolves, after polling `t`
-- with `IO.hasFinished` 3000 times without sleeps. Natively `t` runs on a
-- worker and blocks there, and the program ends. A runtime that runs the
-- polled task on the poller's stack at its polling threshold (1000
-- answers) hangs there (review AR-9; leanrs's probe, native 10 of 10).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let p : IO.Promise Nat ← IO.Promise.new
  let t ← IO.asTask (do
    let v ← IO.wait p.result?
    IO.println s!"t got {v.getD 0}")
  let mut k := 0
  for _ in [0:n] do
    if ← IO.hasFinished t then k := k + 1
  IO.println s!"finished while polling: {k}"
  p.resolve 7
  let _ ← IO.wait t
  IO.println "main done"
