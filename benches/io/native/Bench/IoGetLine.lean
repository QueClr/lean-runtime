/-
`Handle.getLine` over a 64 KiB file of lines of 1 to 79 bytes, rewinding at its end; the twin of
`benches/io/rust/src/bin/io_get_line.rs`.
-/
import Bench.Harness
open Bench

structure Input where
  path : System.FilePath
  h : IO.FS.Handle

def build : IO Input := do
  let p ← tempFile "lines" (randomLines (1 <<< 16))
  return ⟨p, ← IO.FS.Handle.mk p .read⟩

def loop (inp : Input) : Nat → UInt64 → UInt64 → IO UInt64
  | 0, _, acc => pure acc
  | n + 1, x, acc => do
    let x := step x
    let l ← inp.h.getLine
    if l.isEmpty then inp.h.rewind
    loop inp n x (mix acc l.length.toUInt64)

def kernel (inp : Input) (n : UInt64) : IO UInt64 := loop inp n.toNat SEED 0

def main (args : List String) : IO UInt32 :=
  run build kernel (fun inp => IO.FS.removeFile inp.path) args
