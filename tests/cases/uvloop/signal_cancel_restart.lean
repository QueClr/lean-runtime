import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A one-shot SIGUSR1 watcher gets SIGUSR1 while the program computes (no
IO); then `cancel` and `next` (as Std.Async's Signal selector does). Natively
the loop thread delivered it at once: the watcher is finished, `cancel` does
nothing and `next` gives the resolved promise. -/

def busy (n : Nat) : Nat := Id.run do
  let mut acc := 0
  for i in [0:n] do
    acc := (acc + i * 7) % 1000003
  return acc

def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let s ← Signal.mk 10 false
  let _ ← s.next
  let pid ← IO.Process.getPID
  let _ ← IO.Process.spawn { cmd := "sh", args := #["-c", s!"sleep 0.3; kill -USR1 {pid}"] }
  if busy n == 1000004 then IO.println "never"
  s.cancel
  let p2 ← s.next
  IO.sleep 100
  IO.println s!"after cancel and next, resolved: {← IO.hasFinished p2.result?}"
  p2.resolve 0
  s.stop
