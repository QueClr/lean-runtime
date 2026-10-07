import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent runs on the event loop (natively on
libuv's loop thread). It starts an IO task `t` and waits for it with
`IO.waitAny`, so here `t` runs on the loop context's stack, as the task a
free worker would start. `main` returns at 100 ms while `t` sleeps until
about 1 s. Natively `t` runs on a pool worker, which the exit joins:
"main done", then "t done", status 0. Before the fix of review RF13-01
the single-thread scheduler's final run left the loop context suspended
with `t` on it, and "t done" was lost. -/

def main : IO Unit := do
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    let t ← IO.asTask (do IO.sleep 1000; IO.println "t done"; (← IO.getStdout).flush)
    let _ ← IO.waitAny [t]
    pure ()
  IO.sleep 100
  IO.println "main done"
