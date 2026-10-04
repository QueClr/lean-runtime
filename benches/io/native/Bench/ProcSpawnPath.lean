/-
`IO.Process.spawn` of `true`, found through `PATH` (set by the program, as its Rust twin sets it),
every standard stream `null`, then `Child.wait`; the twin of
`benches/io/rust/src/bin/proc_spawn_path.rs`.
-/
import Bench.Harness
open Bench

structure Input where
  cmd : String
  path : String

def once (inp : Input) : IO UInt32 := do
  let c ← IO.Process.spawn { cmd := inp.cmd, env := #[("PATH", some inp.path)], stdin := .null, stdout := .null, stderr := .null }
  c.wait

def loop (inp : Input) : Nat → UInt64 → UInt64 → IO UInt64
  | 0, _, acc => pure acc
  | n + 1, i, acc => do
    let code ← tryCatch (once inp) (fun _ => pure 4294967294)
    loop inp n (i + 1) (mix acc (code.toUInt64 ^^^ i))

def kernel (inp : Input) (n : UInt64) : IO UInt64 := loop inp n.toNat 0 0

def main (args : List String) : IO UInt32 :=
  run (pure ⟨"true", "/usr/local/bin:/usr/bin:/bin"⟩) kernel (fun _ => pure ()) args
