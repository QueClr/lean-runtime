import Std.Internal.UV.Timer
open Std.Internal.UV

/-! `stop` of a repeating timer whose promise has an async dependent (a
control for LB-33: the outcome the correct `sync` order copies). As
`timer_stop_rearm_in_sync_dependent`, with `sync := false`: the dependent
runs on a worker after `stop`, sees a finished timer, and its `next` gives
a promise the timer does not hold, which reads `none` once dropped; so it
runs again, up to CAP, each run recording its value. `main` waits for the
last run, then prints the values.
args: PERIOD (ms) CAP -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

partial def arm (t : Timer) (values : IO.Ref (Array String)) (cap : Nat) (done : IO.Promise Unit) : IO Unit := do
  let p ← t.next
  let _ ← IO.mapTask (sync := false) (fun v => do
      values.modify (·.push (if v.isSome then "some" else "none"))
      if (← values.get).size < cap then arm t values cap done else done.resolve ()) p.result?

def main (args : List String) : IO Unit := do
  let period := args[0]!.toNat!
  let cap := args[1]!.toNat!
  let values ← IO.mkRef (#[] : Array String)
  let done ← IO.Promise.new
  let t ← Timer.mk period.toUInt64 true
  let p0 ← t.next
  let r ← IO.wait p0.result?
  say s!"0th tick: {if r.isSome then "some" else "none"}"
  arm t values cap done
  t.stop
  say "stop: end"
  let _ ← IO.wait done.result?
  say s!"dependent values: {← values.get}"
