import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A deep recursion in a `sync` dependent of a timer's promise (lean-runtime
fixes-16, hunt HSK-03). Natively the dependent runs on libuv's loop
thread, which is made before `LEAN_STACK_SIZE_KB` is read and so has
`lthread`'s default stack, 1 GiB. Here `LEAN_STACK_SIZE_KB=16384` (16 MiB
plus 128 KiB for `main` and the workers), and the recursion (`D` =
2000000 levels) takes 32 to 64 MB in an optimized build, about 220 MB in
the unoptimized one of `scripts/cases.py`: a runtime whose loop follows
the variable overflows. -/

-- Non-tail recursion that allocates nothing.
def deep : Nat → Nat
  | 0 => 0
  | n + 1 => deep n * 3 % 1000003 + 1

def main (args : List String) : IO Unit := do
  let d := args[0]!.toNat!
  let tm ← Timer.mk 100 false
  let p ← tm.next
  -- made long before the timer fires, so it runs where the promise is
  -- resolved: on the loop thread
  let r := p.result!.map (sync := true) fun _ => deep d
  IO.println s!"loop {r.get}"
