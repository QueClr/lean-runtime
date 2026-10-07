/-! `main` on the process's own thread (`LEAN_MAIN_USE_THREAD=0`; the
`ulimit -s 8192` of the `.pipe` line makes its stack 8 MiB) waits for an
IO task with a deep recursion (lean-runtime fixes-16, hunt HSK-02).
Natively the task runs on a worker thread with `lthread`'s stack, 1 GiB.
Its recursion (`D` = 1000000 levels) takes 16 to 32 MB in an optimized
build, about 110 MB in the unoptimized one of `scripts/cases.py`. A
runtime that runs the task on `main`'s stack overflows. -/

-- Non-tail recursion that allocates nothing.
def deep : Nat → Nat
  | 0 => 0
  | n + 1 => deep n * 3 % 1000003 + 1

def main (args : List String) : IO Unit := do
  let d := args[0]!.toNat!
  -- `h` (0) comes from the task, so `deep` runs in the task
  let t ← IO.asTask (do let h ← IO.getNumHeartbeats; pure (deep (d + h)))
  IO.println s!"task {(← IO.wait t).toOption}"
