-- `r.modify f` takes the value out of `r`, then stores `f v` back
-- (`ST.Prim.Ref.modifyUnsafe`: `take`, then `set`), so `r` is empty while `f`
-- runs. Here `f` waits for a task (`slow.get`), on a dedicated task, and
-- `main` sets `r` meanwhile, then reads it. The reference is captured by a
-- task, so it is multi-threaded natively. `modify` is atomic and a completed
-- set is never lost (LB-01): `main`'s set waits for `modify`'s store, then
-- `main` reads 100 at once, and `r` ends 100 (Lean 4.35.0-rc1 does this:
-- `Ref.set` is a `swap`, which waits while the slot is empty). Native Lean
-- 4.34.0's `lean_st_ref_set` is a bare exchange, which stores into the empty
-- slot at once; `modify`'s own set overwrites it later, so `main`'s set is
-- lost (`r` ends 1).

def slowValue (slow : Task (Except IO.Error Nat)) : Nat :=
  match slow.get with
  | .ok n => n
  | .error _ => 0

def main (args : List String) : IO Unit := do
  let setMs := args[0]!.toNat!
  let slowMs := args[1]!.toNat!
  let r ← IO.mkRef (0 : Nat)
  let slow ← IO.asTask (prio := .dedicated) do
    IO.sleep slowMs.toUInt32
    return 1
  let t ← IO.asTask (prio := .dedicated) do
    r.modify fun v => v + slowValue slow
  IO.sleep setMs.toUInt32
  r.set 100
  let t0 ← IO.monoMsNow
  let v ← r.get
  let t1 ← IO.monoMsNow
  let how := if t1 - t0 ≥ (slowMs - setMs) / 2 then "after modify's set" else "at once"
  IO.println s!"get after main's set: {v}, {how}"
  let _ ← IO.wait t
  IO.println s!"after modify: {← r.get}"
