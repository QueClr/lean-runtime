import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A repeating timer with timeout 0 ticks once (libuv's repeat 0 is no
repeat), then its `next` gives a promise that no tick resolves, which the
running timer holds. Natively the loop holds the running timer
(`lean_inc(obj)` at its start) until `stop`, so when the program drops the
timer and that promise, keeping only the promise's task, the task stays
pending: "finished: false". With the timer kept, `stop` lets go of the
promise, whose last reference was the timer's: it reads `none` at once.
Before review HU-02 (fixes-14) both modes of lean-runtime dropped the
running timer after its tick, so the first task read `none`. The argument
is the timers' timeout (0). -/

def dropped (ms : UInt64) : IO (Task (Option Unit)) := do
  let t ← Timer.mk ms true
  let p0 ← t.next
  let _ ← IO.wait p0.result?
  let p1 ← t.next
  return p1.result?

def main (args : List String) : IO Unit := do
  let ms := args[0]!.toNat!.toUInt64
  let task ← dropped ms
  IO.sleep 200
  IO.println s!"dropped: second promise finished: {← IO.hasFinished task}"
  let u ← Timer.mk ms true
  let q0 ← u.next
  let _ ← IO.wait q0.result?
  let q1 ← u.next
  let qt := q1.result?
  IO.sleep 200
  IO.println s!"kept: second promise finished before stop: {← IO.hasFinished qt}"
  u.stop
  IO.println s!"after stop: finished {← IO.hasFinished qt}, value {repr (← IO.wait qt)}"
