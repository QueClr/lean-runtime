-- Float.exp2 on literal operands (a decimal literal and Float.ofBits) and on
-- the same operand from the command line, results printed as bits. Natively
-- glibc's exp2 runs at run time; LLVM folds exp2 of a known operand through
-- the host's pow(2, x), one ulp away from glibc's exp2 on these inputs
-- (found by leanrs's review of the shared crate; lean2rr's folding tests).
-- The literals are the point of this case: it checks that literal operands
-- are not folded.

def main (args : List String) : IO Unit := do
  let a := args.toArray.map String.toNat!
  let f64 (i : Nat) : Float := Float.ofBits a[i]!.toUInt64
  let p64 (name : String) (lit arg : Float) : IO Unit :=
    IO.println s!"{name}: literal {lit.toBits} argv {arg.toBits}"
  p64 "exp2 35.74477454358792" (Float.exp2 35.74477454358792) (Float.exp2 (f64 0))
  p64 "exp2 4630227447100591422" (Float.exp2 (Float.ofBits 4630227447100591422)) (Float.exp2 (f64 1))
  p64 "exp2 4631144597734966002" (Float.exp2 (Float.ofBits 4631144597734966002)) (Float.exp2 (f64 2))
