/-
The harness of lean-runtime's native io micro-benchmarks (`benches/io/README.md`): what every
`Bench/<Name>.lean` shares with its Rust twin in `benches/io/rust/` (the LCG, the inputs, the
checksum), value for value with `benches/io/rust/src/lib.rs`, and the timed entry point.
-/

namespace Bench

/-- Opaque pass-through, so values are forced here and not sunk past the clock reads. -/
@[noinline] def pin {α : Type} (x : α) : BaseIO α := pure x

/-- The LCG both sides draw from: `x' = x * A + C` modulo 2^64. -/
@[inline] def step (x : UInt64) : UInt64 := x * 6364136223846793005 + 1442695040888963407

/-- The LCG's first state in the timed loop. -/
def SEED : UInt64 := 0x9E3779B97F4A7C15

/-- The LCG's first state when the inputs are built. -/
def INPUT_SEED : UInt64 := 0x2545F4914F6CDD1D

/-- The checksum step: rotate and xor. -/
@[inline] def mix (acc r : UInt64) : UInt64 := ((acc <<< 7) ||| (acc >>> 57)) ^^^ r

/-- `size` bytes drawn from the LCG (the byte is the state's top 8 bits). -/
def randomBytes (size : Nat) : ByteArray := Id.run do
  let mut x := INPUT_SEED
  let mut b := ByteArray.emptyWithCapacity size
  for _ in [0:size] do
    x := step x
    b := b.push (x >>> 56).toUInt8
  return b

/-- Lines of 1 to 79 bytes (the newline included), ASCII letters, `size` bytes in all at least. -/
def randomLines (size : Nat) : ByteArray := Id.run do
  let mut x := INPUT_SEED
  let mut b := ByteArray.emptyWithCapacity (size + 81)
  while b.size < size do
    x := step x
    let len := (x >>> 58).toNat + ((x >>> 52) &&& 15).toNat
    for _ in [0:len] do
      x := step x
      b := b.push ((97 : UInt8) + (x >>> 59).toUInt8 % 26)
    b := b.push 10
  return b

/-- A file in the temporary directory, named after the process. -/
def tempFile (tag : String) (contents : ByteArray) : IO System.FilePath := do
  let p : System.FilePath := s!"/tmp/lean-runtime-bench-{tag}-{← IO.Process.getPID}"
  IO.FS.writeBinFile p contents
  return p

/-- Runs one benchmark: N from the single argument, the input built and pinned before the first
clock read, the kernel's result pinned before the second; prints the result on line 1 and
`kernel_ns <ns>` on line 2; `cleanup` runs last. -/
def run {I : Type} (build : IO I) (kernel : I → UInt64 → IO UInt64) (cleanup : I → IO Unit)
    (args : List String) : IO UInt32 := do
  match args.map String.toNat? with
  | [some n] =>
    let input ← pin (← build)
    let n ← pin n.toUInt64
    let t0 ← IO.monoNanosNow
    let out ← pin (← kernel input n)
    let t1 ← IO.monoNanosNow
    IO.println (toString out)
    IO.println s!"kernel_ns {t1 - t0}"
    cleanup input
    return 0
  | _ =>
    IO.eprintln "usage: <benchmark> N"
    return 2

end Bench
