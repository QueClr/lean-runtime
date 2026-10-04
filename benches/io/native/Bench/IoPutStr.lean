/-
`Handle.putStr` of short strings (1 to 64 bytes, chosen by the LCG) on a handle over `/dev/null`;
the twin of `benches/io/rust/src/bin/io_put_str.rs`.
-/
import Bench.Harness
open Bench

structure Input where
  h : IO.FS.Handle
  strs : Array String

def build : IO Input := do
  let h ← IO.FS.Handle.mk "/dev/null" .write
  let mut x := INPUT_SEED
  let mut strs := #[]
  for k in [0:64] do
    let mut s := ""
    for _ in [0:k+1] do
      x := step x
      s := s.push (Char.ofNat (97 + ((x >>> 59) % 26).toNat))
    strs := strs.push s
  return ⟨h, strs⟩

def loop (inp : Input) : Nat → UInt64 → UInt64 → IO UInt64
  | 0, _, acc => pure acc
  | n + 1, x, acc => do
    let x := step x
    let s := inp.strs[(x >>> 58).toNat]!
    inp.h.putStr s
    loop inp n x (mix acc s.utf8ByteSize.toUInt64)

def kernel (inp : Input) (n : UInt64) : IO UInt64 := loop inp n.toNat SEED 0

def main (args : List String) : IO UInt32 := run build kernel (fun _ => pure ()) args
