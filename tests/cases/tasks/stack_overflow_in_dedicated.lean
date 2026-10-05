-- `stack_overflow_in_task` with a dedicated task: the task overflows the
-- stack of its own thread (natively an `lthread` made for it, with the
-- task manager's stack size), and Lean's handler reports it as on any
-- thread (`Stack overflow detected. Aborting.`, SIGABRT, stdout not
-- flushed). `LEAN_STACK_SIZE_KB` keeps the stack small.

def deep : Nat → Nat
  | 0 => 0
  | n + 1 => deep n + 1

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  IO.println "main starts"
  let _ ← IO.asTask (prio := .dedicated) (do IO.println s!"depth {deep n}")
  IO.sleep 1000
  IO.eprintln "not reached"
