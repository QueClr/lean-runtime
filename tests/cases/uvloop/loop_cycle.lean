import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent prints a line, then cycles forever
on the event loop (natively on libuv's loop thread): it starts an IO task
that sleeps 1 ms, waits for it (with `IO.waitAny`, which, unlike `IO.wait`,
prints no "`Task.get` called from a `(sync := true)` task" line, whose count
would race with the exit), then updates an `IO.Ref` for 300 ms. `main`
sleeps 100 ms, prints a line and returns. Natively the exit does not wait
for the loop thread, and the workers end once the queue is empty: both
lines, status 0, at about 0.1 s. Here the same lines, about 1 s later: the
final run counts the loop context's time alone over the whole final run
against its tasks' time plus 1 s. Before, each wait of the callback
(its task, run on the loop context's stack, and waited for as a worker's)
started its budget afresh, and the exit never came (the other user's
review of round 4 of fixes-13). -/

def main : IO Unit := do
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let r ← IO.mkRef (0 : Nat)
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.println "cycling"
    (← IO.getStdout).flush
    repeat do
      let t ← IO.asTask (IO.sleep 1)
      let _ ← IO.waitAny [t]
      let t0 ← IO.monoMsNow
      while (← IO.monoMsNow) - t0 < 300 do r.modify (· + 1)
  IO.sleep 100
  IO.println "main done"
  (← IO.getStdout).flush
