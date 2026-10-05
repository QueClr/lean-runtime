-- Two workers (`LEAN_NUM_THREADS=2`). IO task `ticker` sleeps 100 ms at a
-- time until the program ends: it holds a worker for good. Pure task `p`
-- (about 100 ms of work) is taken by the other worker; then `main` waits
-- for pure task `t`. Natively `p`'s worker takes `t` once `p` ends. A
-- control: a waiter that waited for the ticker's wakes before it let `p`
-- run would wait forever here (leanrs's review LF3-02 of lean-runtime's
-- fixes-3).

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let _ticker ← IO.asTask (do while !(← IO.checkCanceled) do IO.sleep 100)
  IO.sleep 50
  let p := Task.spawn fun _ => slow k
  let _ ← IO.hasFinished p
  let t := Task.spawn fun _ => k % 7
  IO.eprintln s!"t = {t.get}"
