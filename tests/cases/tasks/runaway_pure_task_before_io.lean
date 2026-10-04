-- One worker (`LEAN_NUM_THREADS=1`). A runaway pure task `t` is queued, then
-- an IO task that prints. Natively the only worker takes `t` first (first
-- come, first served) and never finishes it, so the IO task never runs:
-- `main done false false`, then the exit waits for `t` forever. lean-runtime
-- defers pure tasks (docs/sched.md, "The pure-task rule"): the worker only
-- marks `t` started, the IO task runs during `main`'s sleep, and `t` runs at
-- the exit. That outcome, `io task ran` first, is the hand-written
-- alternative `alt1`: lean-runtime's own deviation LSCHED-01 (docs/sched.md,
-- "Known differences from native"; leanrs's DV26 (b)).

partial def spin (x acc : UInt64) : UInt64 :=
  if x == 0 then acc else spin (x * 6364136223846793005 + 1442695040888963407) (acc + 1)

def main (args : List String) : IO Unit := do
  let s := args.head!.toNat!.toUInt64 ||| 1
  let ms := args[1]!.toNat!
  let t := Task.spawn fun _ => spin s 0
  let f1 ← IO.hasFinished t
  let _io ← IO.asTask (IO.eprintln "io task ran")
  IO.sleep ms.toUInt32
  let f2 ← IO.hasFinished t
  IO.eprintln s!"main done {f1} {f2}"
