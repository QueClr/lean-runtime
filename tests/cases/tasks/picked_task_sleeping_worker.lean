-- Two workers (`LEAN_NUM_THREADS=2`). IO task `a` sleeps 200 ms: it holds a
-- worker. Pure task `p` (about a second of work) is taken by the other
-- worker. Then `main` waits for pure task `t`. Natively `a`'s worker is
-- free once `a` ends, and takes `t`: `t` comes while `p` still runs. lean-runtime runs the oldest started pure task for a waiter
-- at once (AR-25), and `p` reaches no yield point, so `t` comes after
-- `p`'s run: the hand-written alternative `alt1`, lean-runtime's own
-- deviation LSCHED-03 (docs/sched.md, "Known differences from native";
-- reviews RF3-02, LF3-05 of fixes-3).

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let a ← IO.asTask (do IO.sleep 200; return 1)
  IO.sleep 20
  let p := Task.spawn fun _ => slow k
  let _ ← IO.hasFinished p
  IO.sleep 20
  let t := Task.spawn fun _ => k + 1
  let v := t.get
  IO.eprintln s!"t = {v}, p finished then: {← IO.hasFinished p}"
  let _ ← IO.wait a
  IO.eprintln s!"p = {p.get}"
