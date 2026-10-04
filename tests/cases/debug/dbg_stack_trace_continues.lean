-- `dbgStackTrace f` prints a stack trace to stderr, then returns `f ()`.
-- Natively the trace is the machine backtrace (`backtrace_symbols`, one line
-- per frame, demangled where Lean can); no translator reproduces it, and the
-- owner's ruling is to print nothing. `normalize` drops the frame lines, so
-- the case checks that the program continues and that nothing else is
-- printed.

@[noinline] def traced (n : Nat) : Nat := dbgStackTrace fun _ => n + 1

def main (args : List String) : IO Unit := do
  let n := args.length
  IO.eprintln "before"
  IO.println s!"value {traced n}"
  IO.eprintln "after"
