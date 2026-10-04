-- One panicking call used at two types. After type erasure the two calls of
-- `gp xs none` are the same, Lean's common-subexpression elimination merges
-- them, and the panic (`xs[5]!` out of bounds: the array's size is the
-- number of arguments, none here) prints once natively. A translation that
-- keeps one instance per type calls `gp` twice and prints it twice
-- (lean2rr cross-test XT-6, leanrs fixture A482, which leanrs refuses:
-- "merged call of a declaration that is not pure-total"). Both translators
-- follow native.
@[noinline] def gp {α} (xs : Array Nat) (x : Option α) : Option α :=
  if xs[5]! > 3 then x else none
@[noinline] def useS (o : Option String) : Nat := match o with | some s => s.length | none => 1
@[noinline] def useF (o : Option (Nat → Nat)) : Nat := match o with | some f => f 3 | none => 2

def main (args : List String) : IO Unit := do
  let xs := Array.range args.length
  IO.println (useS (gp xs none) + useF (gp xs none))
