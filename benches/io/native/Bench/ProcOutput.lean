/-
`IO.Process.output` of `/bin/echo <word>` (a word of 1 to 16 letters chosen by the LCG); the twin
of `benches/io/rust/src/bin/proc_output.rs`.
-/
import Bench.Harness
open Bench

structure Input where
  cmd : String
  words : Array String

def build : IO Input := do
  let mut x := INPUT_SEED
  let mut words := #[]
  for k in [0:16] do
    let mut s := ""
    for _ in [0:k+1] do
      x := step x
      s := s.push (Char.ofNat (97 + ((x >>> 59) % 26).toNat))
    words := words.push s
  return ⟨"/bin/echo", words⟩

def loop (inp : Input) : Nat → UInt64 → UInt64 → IO UInt64
  | 0, _, acc => pure acc
  | n + 1, x, acc => do
    let x := step x
    let w := inp.words[(x >>> 60).toNat]!
    let acc ← try
        let o ← IO.Process.output { cmd := inp.cmd, args := #[w] }
        pure (mix (mix (mix acc o.exitCode.toUInt64) o.stdout.length.toUInt64) o.stderr.length.toUInt64)
      catch _ => pure (~~~acc)
    loop inp n x acc

def kernel (inp : Input) (n : UInt64) : IO UInt64 := loop inp n.toNat SEED 0

def main (args : List String) : IO UInt32 := run build kernel (fun _ => pure ()) args
