import Std.Internal.UV.Signal
open Std.Internal.UV

/-! The descriptors before and after the first signal watcher starts. -/

def fds : IO Nat := do
  return (← System.FilePath.readDir "/proc/self/fd").size

def main : IO Unit := do
  let before ← fds
  let s ← Signal.mk 10 true
  let p ← s.next
  let after ← fds
  IO.println s!"descriptors added by the first watcher: {after - before}"
  p.resolve 0
  s.stop
  IO.println s!"after stop: {(← fds) - before}"
