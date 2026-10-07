import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent prints a line, then reads the clock
forever on the event loop (natively on libuv's loop thread). `main` sleeps
300 ms, prints a line and returns. Natively the exit does not wait for the
loop thread: both lines, status 0 (the single-thread scheduler exits about
1 s after `main` returns: its final run lets the loop context go on for the
time its tasks took, none, plus 1 s). Before the fix of review RF13-03 the
single-thread scheduler's final run and the loop context let each other go
first forever (every clock read is a scheduling point). -/

def main : IO Unit := do
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.println "dependent polls the clock"
    (← IO.getStdout).flush
    repeat discard IO.monoMsNow
  IO.sleep 300
  IO.println "main done"
  (← IO.getStdout).flush
