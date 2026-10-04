-- One worker (`LEAN_NUM_THREADS=1`). `x` holds it for a sleep; `b` and `c`
-- are queued at the default priority, then `a` at `Task.Priority.max`,
-- which waits for `c`. When `x` ends, the worker takes `a` (the highest
-- priority); `a`'s `IO.wait c` raises the worker limit by one
-- (`wait_for`), and the new worker takes the queue's head, `b`, then `c`:
-- `B` before `C` before `A`. A runtime that runs the awaited `c` on `a`'s
-- stack at once prints `C A B` (review AR-10 (i), leanrs's probe).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let x ← IO.asTask (do IO.sleep ms.toUInt32; IO.println "X")
  IO.sleep 20
  let b ← IO.asTask (IO.println "B")
  let c ← IO.asTask (IO.println "C")
  let a ← IO.asTask (prio := .max) (do let _ ← IO.wait c; IO.println "A")
  let _ ← IO.wait a
  let _ ← IO.wait b
  let _ ← IO.wait x
  IO.println "main done"
