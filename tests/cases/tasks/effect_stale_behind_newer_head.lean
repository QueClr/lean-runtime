-- One worker (`LEAN_NUM_THREADS=1`; hunt HSC-02 of lean-runtime). `main`
-- queues `l1` (does nothing) and `l2` (prints "L2"), computes for a while
-- with no scheduling point (the argument sets how long: about 0.5 s
-- natively), queues `h` at `Task.Priority.max` (does nothing), then prints
-- "main". Natively the one worker ran `l1` and then `l2` right after their
-- enqueue, long before `main`'s print; `h` is queued only later. At `main`'s
-- print a task queued 5 ms ago or more goes first in lean-runtime's
-- single-thread scheduler: it runs `l1` there, late, and the worker that
-- `l1` frees takes the newer `h` first, the queue's head then, so "main"
-- comes before "L2" (a known difference, LSCHED-05 of its docs/sched.md).
@[noinline] def spin (n : Nat) (acc : UInt64) : UInt64 := Id.run do
  let mut a := acc
  for i in [0:n] do
    a := a * 6364136223846793005 + i.toUInt64
  return a

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let l1 ← IO.asTask (pure ())
  let l2 ← IO.asTask (IO.println "L2")
  let r := spin n 1
  if r == 42 then IO.println "?"
  let h ← IO.asTask (prio := .max) (pure ())
  IO.println "main"
  let _ ← IO.wait l1
  let _ ← IO.wait l2
  let _ ← IO.wait h
