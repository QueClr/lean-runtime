-- One worker (`LEAN_NUM_THREADS=1`; the review of lean-runtime's fixes-19,
-- the limit of HSC-02's rule). `x`, the worker's task, sleeps 20 ms, queues
-- `h` at `Task.Priority.max` and ends; `l` is queued behind `x`. Natively
-- the worker becomes free only after `h` was queued, and takes `h` before
-- `l`: "H", "L". `main` computes for about 0.5 s natively with no
-- scheduling point (the argument sets how long), then waits for `x`
-- (lean-runtime's single-thread scheduler runs `x` there, late, on `main`'s
-- stack), then flushes its stdout (an effect point with no output), and
-- prints "main" last. A scan at the effect point that passes over `h` as
-- queued too recently starts `l` first.
@[noinline] def spin (n : Nat) (acc : UInt64) : UInt64 := Id.run do
  let mut a := acc
  for i in [0:n] do
    a := a * 6364136223846793005 + i.toUInt64
  return a

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let x ← IO.asTask (do
      IO.sleep 20
      let h ← IO.asTask (prio := .max) (IO.println "H")
      pure h)
  let l ← IO.asTask (IO.println "L")
  let r := spin n 1
  if r == 42 then IO.println "?"
  let h ← IO.ofExcept (← IO.wait x)
  (← IO.getStdout).flush
  let _ ← IO.wait l
  let _ ← IO.wait h
  IO.println "main"
