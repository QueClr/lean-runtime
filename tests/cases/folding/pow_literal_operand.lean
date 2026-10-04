-- Float.pow and Float32.pow with one literal operand (x ^ 0.5, x ^ 2.0,
-- x ^ -1.0, 2.0 ^ y, 10.0 ^ y), the other operand a literal or read from the
-- command line, results printed as bits. Natively Lean's C calls glibc's pow
-- (powf) at run time. LLVM's library-call simplifier rewrites pow with a
-- constant operand into sqrt, x * x, 1 / x, exp2 (or exp10), each one ulp
-- away from glibc's pow on these inputs (leanrs fixture A720 and its finding
-- FLT-1; lean2rr cross-test XT-3). The literals are the point of this case.

-- `x ^ c` and `c ^ y` with a literal `c`, as in leanrs's A720.
def half (x : Float) : Float := x ^ 0.5
def sq (x : Float) : Float := x ^ 2.0
def inv (x : Float) : Float := x ^ Float.ofBits 0xBFF0000000000000
def two (y : Float) : Float := 2.0 ^ y
def ten (y : Float) : Float := 10.0 ^ y
def half32 (x : Float32) : Float32 := x ^ 0.5
def sq32 (x : Float32) : Float32 := x ^ 2.0
def inv32 (x : Float32) : Float32 := x ^ (-1.0)
def two32 (y : Float32) : Float32 := 2.0 ^ y
def ten32 (y : Float32) : Float32 := 10.0 ^ y

def main (args : List String) : IO Unit := do
  let a := args.toArray.map String.toNat!
  let f32 (i : Nat) : Float32 := Float32.ofBits a[i]!.toUInt32
  let p32 (name : String) (lit arg : Float32) : IO Unit :=
    IO.println s!"{name}: literal {lit.toBits} argv {arg.toBits}"
  let f64 (i : Nat) : Float := Float.ofBits a[i]!.toUInt64
  let p64 (name : String) (lit arg : Float) : IO Unit :=
    IO.println s!"{name}: literal {lit.toBits} argv {arg.toBits}"
  p64 "pow half 4607190032495475448" (half (Float.ofBits 4607190032495475448)) (half (f64 0))
  p64 "pow sq 4607189316783422666" (sq (Float.ofBits 4607189316783422666)) (sq (f64 1))
  p64 "pow inv 4613815095151227306" (inv (Float.ofBits 4613815095151227306)) (inv (f64 2))
  p64 "pow two 4600071516463376106" (two (Float.ofBits 4600071516463376106)) (two (f64 3))
  p64 "pow two 4614091398025240904" (two (Float.ofBits 4614091398025240904)) (two (f64 4))
  p64 "pow ten 4617989366598126008" (ten (Float.ofBits 4617989366598126008)) (ten (f64 5))
  p32 "powf half 0x41551907" (half32 (Float32.ofBits 0x41551907)) (half32 (f32 6))
  p32 "powf sq 0x42ba6fe9" (sq32 (Float32.ofBits 0x42ba6fe9)) (sq32 (f32 7))
  p32 "powf inv 0x418a9927" (inv32 (Float32.ofBits 0x418a9927)) (inv32 (f32 8))
  p32 "powf two 0xbf992292" (two32 (Float32.ofBits 0xbf992292)) (two32 (f32 9))
  p32 "powf ten 0xc115e92b" (ten32 (Float32.ofBits 0xc115e92b)) (ten32 (f32 10))
