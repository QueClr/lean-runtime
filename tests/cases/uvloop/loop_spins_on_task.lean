import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent sleeps 300 ms on the event loop
(natively on libuv's loop thread), then starts an IO task that sets a flag,
spins on the flag (reading the clock in each round), reads the clock once
more and prints. `main` returns about 100 ms after the timer starts and
leaves an IO task that computes for about 1 s, calibrated in `main` (the
steps of `busy` per 100 ms in this build, as in `loop_sleep_expired`). A
worker takes the flag's task at once, so the correct outcome is "main
done", then "late", status 0. Native Lean 4.34.0 loses the task's `set`
now and then (LB-01: the spinning `get` puts the old value back), and the
loop thread then spins until the exit: "late" was missing in 1 of 3 runs,
so the expected files are written by hand. With the fourth fix of review
RF13-03 the single-thread scheduler started no queued task while the loop
context ran alone, so the callback spun until its budget ended and lost
the line (RF13-10). The first argument is the length of `calibrate`'s
chunks; the second is the twin's computation, in ms. -/

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
  -- read inside the task, so that the computation stays there
  let len ← IO.mkRef (10 * steps)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.sleep 300
    let flag ← IO.mkRef false
    let _ ← IO.asTask (flag.set true)
    while !(← flag.get) do
      let _ ← IO.monoMsNow
    let _ ← IO.monoMsNow
    IO.println "late"
  -- the timer's dependent has started its sleep when the task starts
  IO.sleep 100
  let _w ← IO.asTask do
    if busy (← len.get) == 1000004 then IO.println "never"
  IO.println "main done"
