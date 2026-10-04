-- A task that overflows its stack: Lean's handler reports it as on any
-- thread (`Stack overflow detected. Aborting.`, SIGABRT, stdout not
-- flushed). `main` sleeps, so the task runs on a context of its own in a
-- one-thread runtime, whose guard page the report must recognize (decisions
-- Q5 checklist). `LEAN_STACK_SIZE_KB` keeps the stack small.

def deep : Nat → Nat
  | 0 => 0
  | n + 1 => deep n + 1

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  IO.println "main starts"
  let _ ← IO.asTask (do IO.println s!"depth {deep n}")
  IO.sleep 1000
  IO.eprintln "not reached"
