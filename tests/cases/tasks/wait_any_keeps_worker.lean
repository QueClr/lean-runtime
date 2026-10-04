-- One worker. `t` takes it and blocks in `IO.waitAny` on a promise; `b` is
-- queued meanwhile. Native `wait_any` does not raise the worker limit (as
-- `wait_for` does), so `b` waits for the worker until `main` resolves the
-- promise and `t` ends: `T done` before `B`. A runtime that frees the
-- worker during `waitAny` runs `b` first (review AR-10 (ii); leanrs's
-- probe).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let p : IO.Promise Nat ← IO.Promise.new
  let t ← IO.asTask (do
    let _ ← IO.waitAny [p.result?]
    IO.println "T done")
  IO.sleep 20
  let b ← IO.asTask (IO.println "B")
  IO.sleep ms.toUInt32
  IO.println "main resolves"
  p.resolve 1
  let _ ← IO.wait t
  let _ ← IO.wait b
  IO.println "main done"
