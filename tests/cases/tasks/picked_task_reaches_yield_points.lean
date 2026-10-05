-- Two workers (`LEAN_NUM_THREADS=2`). `p` (pure, never ends) loops over an
-- `ST.Ref`, so it reads a reference again and again; `q` (pure, quick) is
-- queued after it, then `t` (pure), and `main` waits for `t`. Natively the
-- two workers take `p` and `q`; `q` ends, its worker takes `t`, and `main`
-- prints `t`'s value, then that `p` has not finished and `q` has; the exit
-- then waits for `p` forever. leanrs's review of lean-runtime's fixes-3
-- (LF3-01): the scheduler ran `p` for the waiter (LSCHED-02's choice), and
-- `p`'s reference reads, polling points, never let `q` run, so nothing was
-- printed.

partial def loopST {σ : Type} (r : ST.Ref σ UInt64) (acc : UInt64) : ST σ UInt64 := do
  let x ← r.get
  if x == 0 then return acc
  r.set (x * 6364136223846793005 + 1442695040888963407)
  loopST r (acc + 1)

def spinST (s : UInt64) : UInt64 := runST fun _ => do
  let r ← ST.mkRef s
  loopST r 0

def main (args : List String) : IO Unit := do
  let s := args.head!.toNat!.toUInt64 ||| 1
  let n := args[1]!.toNat!
  let p := Task.spawn fun _ => spinST s
  let _ ← IO.hasFinished p
  let q := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  let _ ← IO.hasFinished q
  let t := Task.spawn fun _ => n + 1
  IO.eprintln s!"t = {t.get}"
  IO.eprintln s!"p finished: {← IO.hasFinished p}, q finished: {← IO.hasFinished q}"
