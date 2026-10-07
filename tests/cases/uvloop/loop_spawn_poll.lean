import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent sleeps 300 ms on the event loop
(natively on libuv's loop thread), starts an IO task that computes for
about 2 s and prints, reads the clock once, then prints. `main` returns
about 100 ms after the timer starts and leaves an IO task that computes
for about 500 ms. The computations have no scheduling point and are
calibrated in `main` (the steps of `busy` per 100 ms in this build, as in
`loop_sleep_expired`). Natively the callback's line comes at once, while
a worker takes the task: "main done", "callback done", then "task done",
status 0. Before, the single-thread scheduler's final run ran the task
when the callback read the clock, then cut the callback after it (the
first two fixes of review RF13-03), or printed the task's line first. The
first argument is the length of `calibrate`'s chunks; the second and
third are the twin's computations of `main`'s and the dependent's task,
in ms. -/

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
  let mainLen ← IO.mkRef (5 * steps)
  let depLen ← IO.mkRef (20 * steps)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.sleep 300
    let _t ← IO.asTask do
      if busy (← depLen.get) == 1000004 then IO.println "never"
      IO.println "task done"
    let _ ← IO.monoMsNow
    IO.println "callback done"
  -- the timer's dependent has started its sleep when the task starts
  IO.sleep 100
  let _w ← IO.asTask do
    if busy (← mainLen.get) == 1000004 then IO.println "never"
  IO.println "main done"
