import Std.Internal.UV.Timer
open Std.Internal.UV

/-! LB-33, `cancel` of a repeating timer. `main` waits for the 0th tick,
then `arm` takes a new promise from `next`; its `sync` dependent records
each value, and subscribes again only in its first run; its second run
resolves `done`. `cancel` releases the promise's last reference: the first
run is inside `cancel`. Correct outcome (lean-runtime's order: the state
before the release; Lean master's `cancel` is unchanged): the timer keeps
running without a promise, so the dependent's `next` stores a new one,
which the timer keeps and the next tick resolves: `main` wakes and prints
`[none, some]`. Native 4.34.0: that promise is overwritten without a
release, so it never resolves, and `main` waits forever (a hang).
args: PERIOD (ms; no tick may come between `arm` and `cancel`) -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

partial def arm (t : Timer) (values : IO.Ref (Array String)) (done : IO.Promise Unit) : IO Unit := do
  let p ← t.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      values.modify (·.push (if v.isSome then "some" else "none"))
      if (← values.get).size == 1 then arm t values done else done.resolve ()) p.result?

def main (args : List String) : IO Unit := do
  let period := args[0]!.toNat!
  let values ← IO.mkRef (#[] : Array String)
  let done ← IO.Promise.new
  let t ← Timer.mk period.toUInt64 true
  let p0 ← t.next
  let r ← IO.wait p0.result?
  say s!"0th tick: {if r.isSome then "some" else "none"}"
  arm t values done
  say "cancel: begin"
  t.cancel
  say "cancel: end"
  let _ ← IO.wait done.result?
  say s!"dependent values: {← values.get}"
  t.stop
  say "stop: end"
