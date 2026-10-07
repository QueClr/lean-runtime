import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent sleeps 500 ms on the event loop
(natively on libuv's loop thread), then prints. `main` returns about
100 ms after the timer starts (after the calibration) and leaves an IO task that computes (no IO, so no scheduling point) for about
1.5 s: `main` first counts the steps of `busy` per 100 ms in this build
(`calibrate`), and the task runs 15 times as many. Natively the loop thread
prints "late" while the task computes, and the exit waits for the task:
"main done", then "late", status 0. Before the fix of review RF13-02 the
single-thread scheduler's final run ran the task, then left the loop
context suspended although its sleep had ended, and "late" was lost. The
first argument is the length of `calibrate`'s chunks; the second is the
twin's computation, in ms. -/

def busy (n : Nat) : Nat := Id.run do
  let mut acc := 0
  for i in [0:n] do
    acc := (acc + i * 7) % 1000003
  return acc

/-- The steps of `busy` per 100 ms in this build: `busy` in chunks of
`chunk` steps until 100 ms have passed, with clock reads between the chunks
(in `main`). Each chunk's length depends on the last one's result, so no
chunk is computed once for all. -/
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
  let len ← IO.mkRef (15 * steps)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.sleep 500
    IO.println "late"
  -- the timer's dependent has started its sleep when the task starts
  IO.sleep 100
  let _w ← IO.asTask do
    if busy (← len.get) == 1000004 then IO.println "never"
  IO.println "main done"
