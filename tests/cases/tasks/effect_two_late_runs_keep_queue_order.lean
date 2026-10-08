-- One worker (`LEAN_NUM_THREADS=1`; the review of lean-runtime's fixes-19,
-- its single window of a freed worker). `main` queues `x1` and `x2`; each
-- queues a task that prints a line ("H1", "H2") and ends. `main` computes
-- for about 0.5 s natively with no scheduling point (the argument sets how
-- long), waits for `x1`, then for `x2` (lean-runtime's single-thread
-- scheduler runs each there, late, on `main`'s stack), flushes its stdout
-- (an effect point with no output), waits for both lines' tasks and prints
-- "main". Natively the one worker takes `x1`, `x2`, then `h1` and `h2` in
-- the queue's order, right after their enqueue: "H1", "H2", "main". An
-- effect point that lets only the tasks of the last freed worker's hold
-- pass starts `h2` before `h1`.
@[noinline] def spin (n : Nat) (acc : UInt64) : UInt64 := Id.run do
  let mut a := acc
  for i in [0:n] do
    a := a * 6364136223846793005 + i.toUInt64
  return a

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let x1 ← IO.asTask (do
      let h ← IO.asTask (IO.println "H1")
      pure h)
  let x2 ← IO.asTask (do
      let h ← IO.asTask (IO.println "H2")
      pure h)
  let r := spin n 1
  if r == 42 then IO.println "?"
  let h1 ← IO.ofExcept (← IO.wait x1)
  let h2 ← IO.ofExcept (← IO.wait x2)
  (← IO.getStdout).flush
  let _ ← IO.wait h1
  let _ ← IO.wait h2
  IO.println "main"
