import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent prints a line, then runs 5 cycles
on the event loop (natively on libuv's loop thread): it starts an IO task
that sleeps 400 ms, waits for it with `IO.waitAny`, then updates an
`IO.Ref` for 300 ms; then it prints "dep done". `main` sleeps 100 ms and
prints a line. Natively the exit waits for the first task's worker and
not for the loop thread: "cycling", "main done", status 0, at about
0.4 s. Here the loop context runs each task on its stack, and the final
run waits for it as for a worker's, a wait that counts for nothing, so
only the polling uses up the loop context's 1 s: the exit comes during the
fourth cycle's polling, at about 2.6 s, with the same lines. Before the
fix of review RF13-13 that wait earned the loop context more time, and all
5 cycles ran ("dep done" at about 3.5 s; an endless version never
exited). -/

def main : IO Unit := do
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let r ← IO.mkRef (0 : Nat)
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.println "cycling"
    (← IO.getStdout).flush
    for _ in [0:5] do
      let t ← IO.asTask (IO.sleep 400)
      let _ ← IO.waitAny [t]
      let t0 ← IO.monoMsNow
      while (← IO.monoMsNow) - t0 < 300 do r.modify (· + 1)
    IO.println "dep done"
  IO.sleep 100
  IO.println "main done"
