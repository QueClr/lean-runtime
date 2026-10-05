-- One worker (`LEAN_NUM_THREADS=1`). It is busy with `busy` (an IO task, 50
-- ms). `t0` (pure, about 100 ms of work) and `t1` (pure, never ends) are
-- queued behind it. `main` waits for `busy`; then the free worker takes
-- `t0` alone and runs it, `main` drops `t1`, which is still queued and is
-- deleted, and `main` waits for `t0`: the line, then exit 0. Review AR-25
-- (from lean2rr's switch to lean-runtime's scheduler, PickDropped): a
-- worker of lean-runtime marked `t0` and `t1` started at once, so `t1`
-- could not be deleted and the exit waited for it forever.

partial def spinForever (n : Nat) : Nat := if n == 0 then 1 else spinForever (n + 1)

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

@[noinline] def mkT1 (k : Nat) : Task Nat := Task.spawn fun _ => spinForever (k + 1)

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let busy ← IO.asTask (do IO.sleep 50; return 1)
  let t0 := Task.spawn fun _ => slow k
  -- pin t0's creation before t1's (the compiler may sink a pure `let`)
  let f0 ← IO.hasFinished t0
  let keep ← IO.mkRef (some (mkT1 k))
  let _ ← IO.wait busy
  -- the worker is free now and (natively) runs t0; drop t1 while it does
  keep.set none
  IO.println s!"t0 {t0.get} {f0}"
