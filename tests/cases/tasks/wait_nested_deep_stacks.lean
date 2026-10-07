/-! Nested waits under deep recursion (lean-runtime fixes-16, hunt HSK-01).
`D K`: `main` recurses `D` levels, then waits with `Task.get` for a new
`Task.spawn` that recurses `D` levels and waits for the next one, `K`
tasks deep. Natively each task runs on a thread of its own, so each
recursion has a whole stack (`LEAN_STACK_SIZE_KB=16384`: 16 MiB plus
128 KiB), of which it takes a quarter at most (about 3.8 MB in the
unoptimized build of `scripts/cases.py`, 0.64 MB in an optimized one). A
runtime that runs each awaited task on its waiter's stack adds the 45
recursions up (about 29 MB optimized), and overflows. `IO.waitAny`'s
chain is `wait_any_nested_deep_stacks`. -/

-- Non-tail recursion: `d` levels, then, at the bottom, the wait for a new
-- task that does the same `top` levels with one task less.
partial def descend (top : Nat) (d k : Nat) : Nat :=
  if d == 0 then
    if k == 0 then 0 else (Task.spawn fun _ => descend top top (k - 1)).get + 1
  else descend top (d - 1) k * 3 % 1000003 + 1

def main (args : List String) : IO Unit := do
  let d := args[0]!.toNat!
  let k := args[1]!.toNat!
  IO.println s!"get chain {descend d d k}"
