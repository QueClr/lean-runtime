import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A repeating 1 ms timer whose tick's `sync` dependent computes for a few
ms (and re-subscribes): the loop is always behind. How long does an extern
(`Timer.mk`) from `main` take? Natively it waits for the loop thread's
current iteration only (review RSIOB-13). The bounds are generous, for a
loaded host: natively `Timer.mk` takes a few ms and about 30 ticks come in
`main`'s 100 ms sleep. The first argument is the native busy loop's length;
the second is the twin's, in ms. -/

def busy (n : Nat) : Nat := Id.run do
  let mut acc := 0
  for i in [0:n] do
    acc := (acc + i * 7) % 1000003
  return acc

partial def arm (t : Timer) (n : IO.Ref Nat) (work : Nat) : IO Unit := do
  let p ← t.next
  let _ ← IO.mapTask (sync := true) (fun _ => do
      if busy work == 1000004 then IO.println "never"
      n.modify (· + 1)
      arm t n work) p.result?

def main (args : List String) : IO Unit := do
  let work := args[0]!.toNat!
  let n ← IO.mkRef 0
  let t ← Timer.mk 1 true
  arm t n work
  IO.sleep 100
  let t0 ← IO.monoNanosNow
  let u ← Timer.mk 1000 false
  let t1 ← IO.monoNanosNow
  let k ← n.get
  IO.println s!"Timer.mk under 2 s: {decide ((t1 - t0) / 1000000 < 2000)}; under 500 ticks so far: {decide (k < 500)}"
  t.stop
  let _ := u
