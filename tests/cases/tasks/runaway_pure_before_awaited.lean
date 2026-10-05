-- Two workers (`LEAN_NUM_THREADS=2`). `p` (pure, never ends) and `q` (pure,
-- quick) are queued, then `t` (pure), and `main` waits for `t`. Natively the
-- two workers take `p` and `q`; `q` ends, its worker takes `t`, and `main`
-- prints `t`'s value, then that `p` has not finished and `q` has (`main`
-- holds both, so neither was deleted); the exit then waits for `p` forever.
-- lean-runtime runs one task at a time: a started pure task keeps its
-- worker until it has run (review AR-25), so `t` waits for a worker, and
-- the waiter's need runs the oldest started pure task, `p`, which never
-- ends: nothing is printed. That outcome is the hand-written alternative
-- `alt1`: lean-runtime's own deviation LSCHED-02 (docs/sched.md, "Known
-- differences from native").

partial def spin (x acc : UInt64) : UInt64 :=
  if x == 0 then acc else spin (x * 6364136223846793005 + 1442695040888963407) (acc + 1)

def main (args : List String) : IO Unit := do
  let s := args.head!.toNat!.toUInt64 ||| 1
  let n := args[1]!.toNat!
  let p := Task.spawn fun _ => spin s 0
  -- pin each task's creation here (the compiler may sink a pure `let`)
  let _ ← IO.hasFinished p
  let q := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  let _ ← IO.hasFinished q
  let t := Task.spawn fun _ => n + 1
  IO.eprintln s!"t = {t.get}"
  IO.eprintln s!"p finished: {← IO.hasFinished p}, q finished: {← IO.hasFinished q}"
