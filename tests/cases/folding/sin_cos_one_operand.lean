-- Float.sin and Float.cos of one operand from the command line, computed
-- together, results printed as bits (review HF-01). Natively the C calls
-- glibc's sin and cos. LLVM can make a sine and a cosine of one operand in
-- one basic block one call of glibc's sincos, whose sine differs from sin's
-- on these inputs: at x = 0x1.ad1fb54442d18p+0 (bits 4610228045947874584)
-- sin gives 4607132368722284764 and sincos 4607132368722284763, and the
-- negatives at -x (bits 13833600082802650392). Each line computes the pair
-- in another shape, each in a function of its own: a function that returns
-- the pair of floats, one that returns the pair of their bits, and one
-- scalar expression (the exclusive or of the bits), where nothing is
-- allocated between the two calls. Natively each function makes its own
-- sin and cos calls. A translation may inline the three and share one pair:
-- lean2rr with lean-runtime 1d5d4d3, where sin and cos were inlined, made
-- them one sincos call, and every line printed sincos's sine.

@[noinline] def floats (x : Float) : Float × Float := (Float.sin x, Float.cos x)

@[noinline] def bitsPair (x : Float) : UInt64 × UInt64 :=
  ((Float.sin x).toBits, (Float.cos x).toBits)

@[noinline] def bitsXor (x : Float) : UInt64 := (Float.sin x).toBits ^^^ (Float.cos x).toBits

def main (args : List String) : IO Unit := do
  for a in args do
    let x := Float.ofBits a.toNat!.toUInt64
    let (s, c) := floats x
    IO.println s!"{a} floats: {s.toBits} {c.toBits}"
    let (sb, cb) := bitsPair x
    IO.println s!"{a} bits: {sb} {cb}"
    IO.println s!"{a} xor: {bitsXor x}"
