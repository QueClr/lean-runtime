/-
`Handle.read k` of 1 to 64 bytes (chosen by the LCG) from a 1 MiB file, rewinding at its end; the
twin of `benches/io/rust/src/bin/io_read.rs`.
-/
import Bench.Harness
open Bench

structure Input where
  path : System.FilePath
  h : IO.FS.Handle

def build : IO Input := do
  let p ← tempFile "read" (randomBytes (1 <<< 20))
  return ⟨p, ← IO.FS.Handle.mk p .read⟩

def loop (inp : Input) : Nat → UInt64 → UInt64 → IO UInt64
  | 0, _, acc => pure acc
  | n + 1, x, acc => do
    let x := step x
    let k : UInt64 := (x >>> 58) + 1
    let b ← inp.h.read k.toUSize
    if b.size.toUInt64 < k then inp.h.rewind
    let first := if h : 0 < b.size then b[0].toUInt64 else 0
    loop inp n x (mix acc (b.size.toUInt64 ^^^ (first <<< 8)))

def kernel (inp : Input) (n : UInt64) : IO UInt64 := loop inp n.toNat SEED 0

def main (args : List String) : IO UInt32 :=
  run build kernel (fun inp => IO.FS.removeFile inp.path) args
