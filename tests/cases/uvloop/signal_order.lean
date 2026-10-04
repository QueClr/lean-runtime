import Std.Internal.UV.Signal
open Std.Internal.UV

/-! Two watchers of SIGUSR1: a one-shot one started first, a repeating one
second, each promise with a `sync` dependent that prints. Natively libuv's
signal tree puts repeating watchers first. -/

def kill (sig : String) : IO Unit := do
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #[s!"-{sig}", toString pid] }

def main : IO Unit := do
  let a ← Signal.mk 10 false
  let b ← Signal.mk 10 true
  let pa ← a.next
  let pb ← b.next
  let ta ← IO.mapTask (sync := true) (fun _ => IO.println "one-shot (started first)") pa.result?
  let tb ← IO.mapTask (sync := true) (fun _ => IO.println "repeating (started second)") pb.result?
  kill "USR1"
  let _ ← IO.wait ta
  let _ ← IO.wait tb
  b.stop
