/-! Nested waits under deep recursion with `IO.waitAny` (lean-runtime
fixes-16, hunt HSK-01). `D K`: `main` recurses `D` levels, then waits with
`IO.waitAny` for a new dedicated task that recurses `D` levels and waits
for the next one, `K` tasks deep. Natively each task runs on a thread of
its own, so each recursion has a whole stack (`LEAN_STACK_SIZE_KB=16384`:
16 MiB plus 128 KiB), of which it takes half at most (about 7.7 MB in the
unoptimized build of `scripts/cases.py`, 0.64 MB in an optimized one). A
runtime that runs each awaited task on its waiter's stack adds the 45
recursions up (about 29 MB optimized), and overflows. `Task.get`'s chain
is `wait_nested_deep_stacks`. -/

-- Non-tail recursion: `d` levels, then, at the bottom, the wait for a new
-- dedicated task that does the same `top` levels with one task less (a
-- dedicated task holds no worker, so `IO.waitAny` may run its one task
-- on its stack).
partial def descendAny (top : Nat) (d k : Nat) : IO Nat := do
  if d == 0 then
    if k == 0 then return 0
    let t ← IO.asTask (prio := .dedicated) (descendAny top top (k - 1))
    match ← IO.waitAny [t] with
    | .ok v => return v + 1
    | .error e => throw e
  else
    let r ← descendAny top (d - 1) k
    return r * 3 % 1000003 + 1

def main (args : List String) : IO Unit := do
  let d := args[0]!.toNat!
  let k := args[1]!.toNat!
  IO.println s!"waitAny chain {← descendAny d d k}"
