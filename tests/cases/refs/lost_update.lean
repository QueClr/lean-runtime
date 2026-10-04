-- A concurrent `IO.Ref.set` must not be lost (LB-01 in docs/lean-bugs.md).
-- In each trial a dedicated task sets the reference to 1 once while `main`
-- reads it in a bounded loop; after `IO.wait` on the task, the reference must
-- read 1. Native Lean 4.34's `lean_st_ref_get` takes the value out and puts
-- it back with an unconditional exchange, so a `set` that lands in between is
-- undone: native loses some trials, a different number each run. The
-- expected output is the correct one.

def trial (bound : Nat) : IO Bool := do
  let r ← IO.mkRef (0 : Nat)
  let t ← IO.asTask (prio := .dedicated) (r.set 1)
  let mut i := 0
  while i < bound do
    if (← r.get) == 1 then break
    i := i + 1
  let _ ← IO.wait t
  return (← r.get) == 0

def main (args : List String) : IO Unit := do
  let trials := args[0]!.toNat!
  let bound := args[1]!.toNat!
  let mut lost := 0
  for _ in [0:trials] do
    if ← trial bound then lost := lost + 1
  IO.println s!"lost {lost}"
