-- `r.modify f` takes the value out of `r`, then stores `f v` back
-- (`ST.Prim.Ref.modifyUnsafe`), so `r` is empty while `f` runs. Here `f`
-- waits for a task (`slow.get`), on a dedicated task, and `main` swaps 100
-- into `r` meanwhile. The reference is captured by a task, so it is
-- multi-threaded natively. A swap is atomic and returns the reference's
-- previous value (LB-18): it waits for `modify`'s store, returns 1, and `r`
-- ends 100. Native Lean 4.34.0's `lean_st_ref_swap` exchanges blindly: on the
-- empty slot it gets its own argument back and returns it at once (100),
-- and `modify`'s store then overwrites it (`r` ends 1). 100 is a scalar, so
-- native's wrong run touches no freed memory.

def slowValue (slow : Task (Except IO.Error Nat)) : Nat :=
  match slow.get with
  | .ok n => n
  | .error _ => 0

def main (args : List String) : IO Unit := do
  let swapMs := args[0]!.toNat!
  let slowMs := args[1]!.toNat!
  let r ← IO.mkRef (0 : Nat)
  let slow ← IO.asTask (prio := .dedicated) do
    IO.sleep slowMs.toUInt32
    return 1
  let t ← IO.asTask (prio := .dedicated) do
    r.modify fun v => v + slowValue slow
  IO.sleep swapMs.toUInt32
  let t0 ← IO.monoMsNow
  let old ← r.swap 100
  let t1 ← IO.monoMsNow
  let how := if t1 - t0 ≥ (slowMs - swapMs) / 2 then "after modify's set" else "at once"
  IO.println s!"swap during modify returned: {old}, {how}"
  let _ ← IO.wait t
  IO.println s!"after modify: {← r.get}"
