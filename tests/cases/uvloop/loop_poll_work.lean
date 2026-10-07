import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer's `sync` dependent updates an `IO.Ref` for about
1.5 s on the event loop (natively on libuv's loop thread), then prints.
`main` returns about 100 ms after the timer starts and leaves an IO task
that computes for about 4 s. Both lengths are calibrated in `main`: the
updates per 100 ms of the same reference, shared with a task first as the
callback's closure shares it, and the steps of `busy` per 100 ms in this
build (as in `loop_sleep_expired`). Natively the loop thread prints while the task
computes: "main done", then "late true", status 0. Here the reference's
reads are scheduling points. Before, the single-thread scheduler's final
run let the loop context go on for 1 s at most, and the callback, longer
than that (and slowed by switches to `main` at its scheduling points), was
cut: the line was lost. The first argument is the length of `calibrate`'s
chunks; the second is the twin's `main` task, in ms (the twin calibrates
its callback's updates as here). -/

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

/-- The updates of `r` per 100 ms in this build, in chunks of `chunk`. -/
partial def calibrateRef (r : IO.Ref Nat) (chunk t0 updates : Nat) : IO Nat := do
  for _ in [0:chunk] do r.modify (· + 1)
  let updates := updates + chunk
  if (← IO.monoMsNow) - t0 ≥ 100 then return updates
  calibrateRef r chunk t0 updates

def main (args : List String) : IO Unit := do
  let chunk := args[0]!.toNat!
  let steps ← calibrate chunk (← IO.monoMsNow) 0 0
  let r ← IO.mkRef (0 : Nat)
  -- a task shares `r` first, as the callback's closure does: the
  -- calibration then updates a reference of the same kind (natively a
  -- shared one is slower)
  let warm ← IO.asTask (r.modify (· + 0))
  let _ ← IO.wait warm
  let updates ← calibrateRef r 10000 (← IO.monoMsNow) 0
  r.set 0
  let k := 15 * updates
  -- read inside the task, so that the computation stays there
  let len ← IO.mkRef (40 * steps)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    for _ in [0:k] do r.modify (· + 1)
    IO.println s!"late {(← r.get) == k}"
  -- the timer's dependent runs meanwhile, and goes on during the task
  IO.sleep 100
  let _w ← IO.asTask do
    if busy (← len.get) == 1000004 then IO.println "never"
  IO.println "main done"
