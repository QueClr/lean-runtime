-- Two workers (`LEAN_NUM_THREADS=2`). A watchdog IO task sleeps 3 s, then
-- fails the program unless it is shutting down (it holds a worker while it
-- sleeps). Pure task `p` (about 100 ms of work) is taken by the other
-- worker; `main` waits for pure task `t`, queued behind it. Natively `p`'s
-- worker takes `t` once `p` ends: `t = 2`, then the exit waits for the
-- watchdog, which sees the shutdown: status 0. leanrs's review LF3-05 of
-- lean-runtime's fixes-3: a waiter that waited for a worker-holding
-- sleeper's wake before it let `p` run idled the only thread for the
-- watchdog's 3 s, and the watchdog fired: `timeout`, status 1.

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let _wd ← IO.asTask (do
    IO.sleep 3000
    if !(← IO.checkCanceled) then
      IO.eprintln "timeout"
      IO.Process.exit 1)
  IO.sleep 50
  let p := Task.spawn fun _ => slow k
  let _ ← IO.hasFinished p
  let t := Task.spawn fun _ => k % 7
  IO.eprintln s!"t = {t.get}"
