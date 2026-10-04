import Std.Internal.UV.Signal
open Std.Internal.UV

/-! (a): after a SIGIO (29) watcher stops, SIGIO has its default action
again (Linux: terminate, status 157). -/

def main : IO Unit := do
  let s ← Signal.mk 29 true
  let _ ← s.next
  s.stop
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #["-IO", toString pid] }
  IO.sleep 200
  IO.println "survived SIGIO"
