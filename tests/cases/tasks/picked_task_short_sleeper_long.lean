-- Two workers (`LEAN_NUM_THREADS=2`). IO task `ticker` sleeps 1 s once: it
-- holds a worker. Pure task `p` (a few ms of work) is taken by the other
-- worker; then `main` waits for pure task `t`. Natively `p`'s worker
-- finishes `p` at once and takes `t`: `t` comes long before 500 ms, then
-- the tick. A control: a waiter that waited for the ticker's wake before
-- it let `p` run delayed `t` past the tick (review RF3-05 of lean-runtime's
-- fixes-3).

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let t0 ← IO.monoMsNow
  let ticker ← IO.asTask (do IO.sleep 1000; IO.eprintln "tick"; return 1)
  IO.sleep 50
  let p := Task.spawn fun _ => slow k
  let _ ← IO.hasFinished p
  let t := Task.spawn fun _ => k % 7
  let v := t.get
  IO.eprintln s!"t = {v}, before 500 ms: {decide ((← IO.monoMsNow) - t0 < 500)}"
  let _ ← IO.wait ticker
  IO.eprintln s!"p = {p.get}"
