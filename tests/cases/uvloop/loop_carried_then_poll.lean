import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent starts an IO task that sleeps
1.2 s, waits for it with `IO.waitAny`, reads the clock and prints, on the
event loop (natively on libuv's loop thread, while a worker runs the
task, which the exit waits for). `main` sleeps 100 ms and prints a line.
Natively: "main done", then "after", status 0. Here the task runs on the
loop context's stack, and the final run waits for it as for a worker's,
a wait that counts for nothing against the loop context's time. With the
sixth fix of review RF13-03 that wait used up the loop context's time, and
the callback was cut at its clock read: "after" was lost (RF13-14). -/

def main : IO Unit := do
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    let t ← IO.asTask (IO.sleep 1200)
    let _ ← IO.waitAny [t]
    let _ ← IO.monoMsNow
    IO.println "after"
  IO.sleep 100
  IO.println "main done"
