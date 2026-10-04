-- `r.modify f` takes the value out of `r`, then sets `f v` back
-- (`ST.Prim.Ref.modifyUnsafe`: `take`, then `set`), so `r` is empty while `f`
-- runs. Here `f` waits for a task (`slow.get`), on a dedicated task, and
-- `main` reads `r` meanwhile. The reference is captured by a task, so it is
-- multi-threaded natively, and `lean_st_ref_get` spins while the slot is
-- empty: `main`'s read waits for `modify`'s set and returns its value. A
-- runtime that runs tasks on one thread must block the read until the next
-- set, letting the other tasks run (docs/sched.md, The glue, item 7).

def slowValue (slow : Task (Except IO.Error Nat)) : Nat :=
  match slow.get with
  | .ok n => n
  | .error _ => 0

def main (args : List String) : IO Unit := do
  let readMs := args[0]!.toNat!
  let slowMs := args[1]!.toNat!
  let r ← IO.mkRef (0 : Nat)
  let slow ← IO.asTask (prio := .dedicated) do
    IO.sleep slowMs.toUInt32
    return 1
  let t ← IO.asTask (prio := .dedicated) do
    r.modify fun v => v + slowValue slow
  IO.sleep readMs.toUInt32
  let t0 ← IO.monoMsNow
  let v ← r.get
  let t1 ← IO.monoMsNow
  let how := if t1 - t0 ≥ (slowMs - readMs) / 2 then "after modify's set" else "at once"
  IO.println s!"get during modify: {v}, {how}"
  let _ ← IO.wait t
  IO.println s!"after modify: {← r.get}"
