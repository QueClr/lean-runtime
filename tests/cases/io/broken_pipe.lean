-- Writing to a pipe whose reader has gone (`| head -1` in .pipe) is an IO
-- error (`resource vanished`, EPIPE; Lean ignores SIGPIPE) raised by the
-- `putStr` whose buffer flush fails; uncaught, it ends the program with exit
-- code 1. The line count comes from argv (from lean2rr's
-- tests/runtime/RtBrokenPipe.lean).

def main (args : List String) : IO Unit := do
  for i in [0:args.head!.toNat!] do
    IO.println s!"line {i}"
  IO.eprintln "finished loop"
