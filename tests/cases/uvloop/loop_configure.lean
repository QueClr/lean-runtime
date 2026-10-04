import Std.Internal.UV.Loop
open Std.Internal.UV

/-! `Std.Internal.UV.Loop.configure` and `Loop.alive`: native Lean's loop
always has its async handle, so it is alive; configuring it (idle-time
accounting, `SIGPROF` blocked while polling) changes nothing a program sees.
The options come from argv. -/

def main (args : List String) : IO Unit := do
  IO.println s!"alive: {← Loop.alive}"
  Loop.configure { accumulateIdleTime := args[0]! == "1", blockSigProfSignal := args[1]! == "1" }
  IO.println s!"configured, alive: {← Loop.alive}"
  Loop.configure {}
  IO.println s!"alive: {← Loop.alive}"
