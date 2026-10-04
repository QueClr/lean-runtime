import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A one-shot signal watcher and two SIGUSR1 back to back, while `main`
computes without waiting: natively the first signal resets the handler
(libuv's `SA_RESETHAND` for a one-shot watcher), so the second takes the
default action and ends the program (status 138), whether or not the loop
has delivered the first yet. A child sends both, 50 ms apart; `main` spins
on a pure computation meanwhile. The signal number and the spin's length
come from argv. -/

/-- `n` steps of an LCG from `x`. -/
def spin (n x : UInt64) : UInt64 := Id.run do
  let mut x := x
  for _ in [0:n.toNat] do
    x := x * 6364136223846793005 + 1442695040888963407
  return x

def main (args : List String) : IO Unit := do
  let s ← Signal.mk (Int32.ofInt args[0]!.toInt!) false
  let p ← s.next
  let pid ← IO.Process.getPID
  let _child ← IO.Process.spawn { cmd := "sh", args := #["-c", s!"kill -USR1 {pid}; sleep 0.05; kill -USR1 {pid}"] }
  IO.println "before"
  (← IO.getStdout).flush
  -- seconds of pure computation, never a yield point
  let n := spin args[1]!.toNat!.toUInt64 (pid.toUInt64 ||| 1)
  IO.println s!"not reached {n} {← IO.hasFinished p.result?}"
