-- One worker (`LEAN_NUM_THREADS=1`; the review of lean-runtime's fixes-19).
-- `main` queues `l1` (does nothing) and `l2` (prints "L2"), computes for
-- about 0.5 s natively with no scheduling point (the argument sets how
-- long), queues `h` at `Task.Priority.max` (prints "H"), then prints
-- "main". Natively the one worker ran `l1` and `l2` right after their
-- enqueue, and takes `h` only after `main`'s print: "L2", "main", "H".
-- lean-runtime's single-thread scheduler runs `l1` late, at `main`'s print,
-- after `h` was queued, and the worker it frees then takes `h` before `l2`:
-- "main", "H", "L2" (a known difference, LSCHED-05 of its docs/sched.md).
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
  let h ← IO.asTask (prio := .max) (IO.println "H")
  IO.println "main"
  let _ ← IO.wait l1
  let _ ← IO.wait l2
  let _ ← IO.wait h
