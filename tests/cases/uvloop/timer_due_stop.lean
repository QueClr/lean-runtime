import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot 10 ms timer becomes due while the program computes (no IO);
then `stop`. Natively the loop thread fired it at 10 ms: its promise holds
`()`. -/

def busy (n : Nat) : Nat := Id.run do
  let mut acc := 0
  for i in [0:n] do
    acc := (acc + i * 7) % 1000003
  return acc

def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let t ← Timer.mk 10 false
  let p ← t.next
  if busy n == 1000004 then IO.println "never"
  t.stop
  IO.println s!"due timer, then stop: {repr (← IO.wait p.result?)}"
