-- Two workers (`LEAN_NUM_THREADS=2`). `p` (pure, never ends) calls
-- `dbgSleep 0` at every step, a yield point; `q` (pure, quick) is queued
-- after it, then `t` (pure), and `main` waits for `t`. Natively the two
-- workers take `p` and `q`; `q` ends, its worker takes `t`, and `main`
-- prints `t`'s value, then that `p` has not finished and `q` has; the exit
-- then waits for `p` forever. leanrs's review LF3-04 of lean-runtime's
-- fixes-3: `p` ran for the waiter, and its zero sleeps never let `q` run,
-- so nothing was printed.

partial def spinSleep (x acc : UInt64) : UInt64 :=
  if x == 0 then acc else
    let x' := dbgSleep 0 fun _ => x * 6364136223846793005 + 1442695040888963407
    spinSleep x' (acc + 1)

def main (args : List String) : IO Unit := do
  let s := args.head!.toNat!.toUInt64 ||| 1
  let n := args[1]!.toNat!
  let p := Task.spawn fun _ => spinSleep s 0
  let _ ← IO.hasFinished p
  let q := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  let _ ← IO.hasFinished q
  let t := Task.spawn fun _ => n + 1
  IO.eprintln s!"t = {t.get}"
  IO.eprintln s!"p finished: {← IO.hasFinished p}, q finished: {← IO.hasFinished q}"
