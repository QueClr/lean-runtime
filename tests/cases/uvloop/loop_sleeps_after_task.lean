import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent sleeps 300 ms on the event loop
(natively on libuv's loop thread), prints "a", sleeps 100 ms, then prints
"b". `main` returns about 100 ms after the timer starts and leaves an IO
task that computes for about 1.5 s, calibrated in `main` (the steps of
`busy` per 100 ms in this build, as in `loop_sleep_expired`). Natively the
loop thread prints both lines while the task computes: "main done", "a",
"b", status 0. Before the fix of review RF13-12 the single-thread
scheduler's final run let the loop context print "a", then left it
suspended in its second sleep: "b" was lost. The first argument is the
length of `calibrate`'s chunks; the second is the twin's computation, in
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
  -- read inside the task, so that the computation stays there
  let len ← IO.mkRef (15 * steps)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    IO.sleep 300
    IO.println "a"
    IO.sleep 100
    IO.println "b"
  -- the timer's dependent has started its sleep when the task starts
  IO.sleep 100
  let _w ← IO.asTask do
    if busy (← len.get) == 1000004 then IO.println "never"
  IO.println "main done"
