import Std.Internal.UV.Timer
open Std.Internal.UV

/-! `main` returns while a `sync := true` dependent of a one-shot timer's
promise, which runs on the event loop (natively on libuv's loop thread),
sleeps in a loop. Natively the exit does not wait for the loop thread: the
dependent's line, `main`'s stderr line, `main`'s last stdout line (written
by the exit's flush) and `main`'s status 3. Before the fix of review
HL2-02 the single-thread scheduler's final run waited for the loop
context, for good. -/

def main : IO UInt32 := do
  let t ← Timer.mk 10 false
  let p ← t.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.println "dependent runs on the loop"
    (← IO.getStdout).flush
    repeat IO.sleep 1000
  IO.sleep 1000
  IO.eprintln "main's stderr line"
  IO.println "main done"
  return 3
