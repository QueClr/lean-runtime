-- One worker (`LEAN_NUM_THREADS=1`; the other translator's second review of
-- lean-runtime's fixes-19). `main` makes a reference holding 0, queues `l1`
-- (prints the value it reads) and `l2` (prints "L2"), computes for about
-- 0.5 s natively with no scheduling point (the argument sets how long),
-- queues `h` at `Task.Priority.max` (prints "H"), sets the reference to 1
-- and prints "main". Natively the one worker ran `l1` and `l2` right after
-- their enqueue: "l1 saw 0", "L2", "main", "H". `l1` can read 1 only if the
-- worker ran it after `h` was queued; then the worker takes `h` before
-- `l2`, so "L2" before "H" goes with "l1 saw 0" in every native schedule.
@[noinline] def spin (n : Nat) (acc : UInt64) : UInt64 := Id.run do
  let mut a := acc
  for i in [0:n] do
    a := a * 6364136223846793005 + i.toUInt64
  return a

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let r ← IO.mkRef 0
  let l1 ← IO.asTask (do
      let v ← r.get
      IO.println s!"l1 saw {v}")
  let l2 ← IO.asTask (IO.println "L2")
  let s := spin n 1
  if s == 42 then IO.println "?"
  let h ← IO.asTask (prio := .max) (IO.println "H")
  r.set 1
  IO.println "main"
  let _ ← IO.wait l1
  let _ ← IO.wait l2
  let _ ← IO.wait h
