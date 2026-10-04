-- Float32 libm functions on a literal operand and on the same operand read
-- from the command line, results printed as bits. Natively Lean's C calls
-- glibc's float function (cosf, sinf, ...) at run time in both cases (a
-- closed term sits in a once-cell the C compiler cannot see into). LLVM
-- folds a call whose operand it knows by evaluating the double function and
-- rounding to float, one ulp away from glibc's on these inputs (searched with
-- glibc 2.39 on aarch64), so a translation that leaves the call visible to
-- LLVM gets the literal lines wrong (lean2rr cross-test XT-4, leanrs prims
-- float32 `_const` rows and its numeric-semantics note N7). The last two
-- lines are atan2 and pow on two literals, folded the same way. The literals
-- are the point of this case: it checks that literal operands are not folded.

def main (args : List String) : IO Unit := do
  let a := args.toArray.map String.toNat!
  let f32 (i : Nat) : Float32 := Float32.ofBits a[i]!.toUInt32
  let p32 (name : String) (lit arg : Float32) : IO Unit :=
    IO.println s!"{name}: literal {lit.toBits} argv {arg.toBits}"
  p32 "sin 0x3f19c612" (Float32.sin (Float32.ofBits 0x3f19c612)) (Float32.sin (f32 0))
  p32 "sin 0x3d525610" (Float32.sin (Float32.ofBits 0x3d525610)) (Float32.sin (f32 1))
  p32 "cos 0x3f88c811" (Float32.cos (Float32.ofBits 0x3f88c811)) (Float32.cos (f32 2))
  p32 "cos 0x3dce4fee" (Float32.cos (Float32.ofBits 0x3dce4fee)) (Float32.cos (f32 3))
  p32 "tan 0x3f56cf56" (Float32.tan (Float32.ofBits 0x3f56cf56)) (Float32.tan (f32 4))
  p32 "asin 0x3f22fcc1" (Float32.asin (Float32.ofBits 0x3f22fcc1)) (Float32.asin (f32 5))
  p32 "acos 0x3f13ed6a" (Float32.acos (Float32.ofBits 0x3f13ed6a)) (Float32.acos (f32 6))
  p32 "atan 0x4033b00b" (Float32.atan (Float32.ofBits 0x4033b00b)) (Float32.atan (f32 7))
  p32 "sinh 0x4087ada2" (Float32.sinh (Float32.ofBits 0x4087ada2)) (Float32.sinh (f32 8))
  p32 "sinh 0x3c036224" (Float32.sinh (Float32.ofBits 0x3c036224)) (Float32.sinh (f32 9))
  p32 "cosh 0x403fc55f" (Float32.cosh (Float32.ofBits 0x403fc55f)) (Float32.cosh (f32 10))
  p32 "cosh 0x3e00944e" (Float32.cosh (Float32.ofBits 0x3e00944e)) (Float32.cosh (f32 11))
  p32 "tanh 0x3fef4398" (Float32.tanh (Float32.ofBits 0x3fef4398)) (Float32.tanh (f32 12))
  p32 "asinh 0x40aa87de" (Float32.asinh (Float32.ofBits 0x40aa87de)) (Float32.asinh (f32 13))
  p32 "acosh 0x402dd0ac" (Float32.acosh (Float32.ofBits 0x402dd0ac)) (Float32.acosh (f32 14))
  p32 "atanh 0x3e9a4996" (Float32.atanh (Float32.ofBits 0x3e9a4996)) (Float32.atanh (f32 15))
  p32 "exp 0xbfe53b48" (Float32.exp (Float32.ofBits 0xbfe53b48)) (Float32.exp (f32 16))
  p32 "exp2 0xbf992292" (Float32.exp2 (Float32.ofBits 0xbf992292)) (Float32.exp2 (f32 17))
  p32 "exp2 0x3c1db9aa" (Float32.exp2 (Float32.ofBits 0x3c1db9aa)) (Float32.exp2 (f32 18))
  p32 "log 0x42531480" (Float32.log (Float32.ofBits 0x42531480)) (Float32.log (f32 19))
  p32 "log 0x3e1978a0" (Float32.log (Float32.ofBits 0x3e1978a0)) (Float32.log (f32 20))
  p32 "log2 0x3f733c0e" (Float32.log2 (Float32.ofBits 0x3f733c0e)) (Float32.log2 (f32 21))
  p32 "log10 0x40e7550a" (Float32.log10 (Float32.ofBits 0x40e7550a)) (Float32.log10 (f32 22))
  p32 "log10 0x3c043aad" (Float32.log10 (Float32.ofBits 0x3c043aad)) (Float32.log10 (f32 23))
  p32 "cbrt 0x3f8ee836" (Float32.cbrt (Float32.ofBits 0x3f8ee836)) (Float32.cbrt (f32 24))
  p32 "atan2f 0x3f9c337a 0x400a5a20" (Float32.atan2 (Float32.ofBits 0x3f9c337a) (Float32.ofBits 0x400a5a20)) (Float32.atan2 (f32 25) (f32 26))
  p32 "powf 0x3fe79089 0x3dcdfa40" (Float32.pow (Float32.ofBits 0x3fe79089) (Float32.ofBits 0x3dcdfa40)) (Float32.pow (f32 27) (f32 28))
