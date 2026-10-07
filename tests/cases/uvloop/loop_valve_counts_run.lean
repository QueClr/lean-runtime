import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent sleeps 300 ms on the event loop
(natively on libuv's loop thread), then starts an IO task that computes for
about 1.5 s, reads the clock once and prints. `main` returns about 100 ms
after the timer starts and leaves an IO task that computes for about 1 s.
Both computations have no scheduling point and are calibrated in `main`
(the steps of `busy` per 100 ms in this build, as in `loop_sleep_expired`).
Natively the loop thread prints while the tasks compute: "main done", then
"late", status 0. With the second fix of review RF13-03 the single-thread
scheduler's final run counted the wall clock of the dependent's task, run
on `main`'s stack, against the loop context's 1 s, and stopped it at its
clock read (RF13-07): "late" was lost. The first argument is the length of
`calibrate`'s chunks; the second and third are the twin's computations, in
ms. -/

def busy (n : Nat) : Nat := Id.run do
  let mut acc := 0
  for i in [0:n] do
    acc := (acc + i * 7) % 1000003
  return acc

/-- The steps of `busy` per 100 ms in this build (as in
`loop_sleep_expired`). -/
partial def calibrate (chunk t0 steps acc : Nat) : IO Nat := do
  let acc := busy (chunk + acc % 2)
  let steps := steps + chunk
  if (← IO.monoMsNow) - t0 ≥ 100 then
    if acc == 1000004 then IO.println "never"
    return steps
  calibrate chunk t0 steps acc

def main (args : List String) : IO Unit := do
  let chunk := args[0]!.toNat!
  let steps ← calibrate chunk (← IO.monoMsNow) 0 0
  -- read inside the tasks, so that the computations stay there
  let mainLen ← IO.mkRef (10 * steps)
  let depLen ← IO.mkRef (15 * steps)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.sleep 300
    let _ ← IO.asTask do
      if busy (← depLen.get) == 1000004 then IO.println "never"
    let _ ← IO.monoMsNow
    IO.println "late"
  -- the timer's dependent has started its sleep when the task starts
  IO.sleep 100
  let _w ← IO.asTask do
    if busy (← mainLen.get) == 1000004 then IO.println "never"
  IO.println "main done"
