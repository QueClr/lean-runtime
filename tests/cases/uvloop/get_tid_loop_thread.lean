import Std.Internal.UV.Timer
open Std.Internal.UV

/-! `IO.getTID` in a `sync` dependent of a timer's promise: it runs where
the promise is resolved, on the event loop's thread, which natively is one
thread for the whole program (`libuv.cpp`, `initialize_libuv`): the same id
for both timers, though they fire 50 ms apart, and neither `main`'s nor a
pool worker's (review AR-37 of lean-runtime). Only relations are printed:
the ids change from run to run. -/
def main : IO Unit := do
  let mt ← IO.getTID
  let a ← IO.wait (← IO.asTask IO.getTID)
  let t1 ← Timer.mk 20 false
  let p1 ← t1.next
  let s1 ← IO.mapTask (sync := true) (fun _ => IO.getTID) p1.result?
  let x ← IO.wait s1
  IO.sleep 50
  let t2 ← Timer.mk 20 false
  let p2 ← t2.next
  let s2 ← IO.mapTask (sync := true) (fun _ => IO.getTID) p2.result?
  let y ← IO.wait s2
  match a, x, y with
  | .ok a, .ok x, .ok y =>
    IO.println s!"both timers' dependents on one thread: {x == y}"
    IO.println s!"not main's: {x != mt}"
    IO.println s!"not the pool worker's: {x != a}"
  | _, _, _ => IO.println "error"
