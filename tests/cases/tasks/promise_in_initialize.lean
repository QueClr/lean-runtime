-- `IO.Promise.new` in an `initialize` declaration, before the task manager
-- runs: `lean_promise_new` with no task manager is Lean's internal panic
-- (object.cpp 1310-1316), `INTERNAL PANIC: ` and its message on stderr,
-- then `exit(1)`, and `main` never runs (review RSH3-05 of shared-3).
-- An earlier `initialize` leaves `pending;` in stdout's buffer, which
-- `exit` writes after the line.

initialize IO.print "pending;"

initialize p : IO.Promise Nat ← IO.Promise.new

def main (args : List String) : IO Unit := do
  IO.println s!"main {args.length}"
