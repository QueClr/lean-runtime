/-
`IO.Process.spawn` of `/bin/true` in `/` with every standard stream `null`, then `Child.wait`; the
twin of `benches/io/rust/src/bin/proc_spawn_cwd.rs`.
-/
import Bench.Harness
open Bench

def once (cmd : String) : IO UInt32 := do
  let c ← IO.Process.spawn { cmd, cwd := some "/", stdin := .null, stdout := .null, stderr := .null }
  c.wait

def loop (cmd : String) : Nat → UInt64 → UInt64 → IO UInt64
  | 0, _, acc => pure acc
  | n + 1, i, acc => do
    let code ← tryCatch (once cmd) (fun _ => pure 4294967294)
    loop cmd n (i + 1) (mix acc (code.toUInt64 ^^^ i))

def kernel (cmd : String) (n : UInt64) : IO UInt64 := loop cmd n.toNat 0 0

def main (args : List String) : IO UInt32 := run (pure "/bin/true") kernel (fun _ => pure ()) args
