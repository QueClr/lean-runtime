-- One worker (`LEAN_NUM_THREADS=1`). Pure task `p0` (a few ms of work,
-- awaited at the end), then pure task `p1` (never ends; kept in a
-- reference), then an IO task that prints. Natively the only worker takes
-- `p0`, then `p1`, which it never finishes: the IO task never runs, and
-- `main`, which drops `p1` too late (it has started) and waits for the IO
-- task, waits forever. lean-runtime: `p0` is started and keeps the worker
-- (AR-25); the IO task passes over `p1`, which waits for a worker
-- (LSCHED-01), and runs; `p1` is still queued when `main` drops it, so it
-- is deleted, and the program prints `p0`'s value and ends. That outcome is
-- the hand-written alternative `alt1`: lean-runtime's own deviation
-- LSCHED-01 (docs/sched.md, "Known differences from native"; leanrs's
-- review LF3-03 of fixes-3).

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

partial def spinForever (n : Nat) : Nat := if n == 0 then 1 else spinForever (n + 1)

@[noinline] def mkP1 (k : Nat) : Task Nat := Task.spawn fun _ => spinForever (k + 1)

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let p0 := Task.spawn fun _ => slow k
  let _ ← IO.hasFinished p0
  let keep ← IO.mkRef (some (mkP1 k))
  let io ← IO.asTask (IO.eprintln "io")
  IO.sleep 300
  keep.set none
  let _ ← IO.wait io
  IO.eprintln s!"done {p0.get}"
