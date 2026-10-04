import Std.Internal.UV.Signal
import Std.Internal.UV.Timer
open Std.Internal.UV

/-! main returns while a repeating watcher listens and a repeating timer
runs, both dropped by the program, each promise with a `sync` dependent that
prints. Natively nothing runs at exit. -/

def main : IO Unit := do
  let s ← Signal.mk 10 true
  let p ← s.next
  let _ ← IO.mapTask (sync := true) (fun r => IO.eprintln s!"signal dependent ran: {repr r}") p.result?
  let t ← Timer.mk 100000 true
  let q ← t.next
  let _ ← IO.wait q.result?
  let q2 ← t.next
  let _ ← IO.mapTask (sync := true) (fun r => IO.eprintln s!"timer dependent ran: {repr r}") q2.result?
  IO.println "main returns"
