-- `IO.Process.exit` from inside nested IO code flushes buffered stdout and
-- exits with the given status; `IO.Process.forceExit` (`_Exit`) loses
-- buffered stdout and keeps the unbuffered stderr. The mode, counts and
-- status come from argv (from lean2rr's tests/runtime/RtExit.lean and
-- RtForceExit.lean).

def work (n stop : Nat) (code : UInt8) : IO Unit := do
  for i in [0:n] do
    IO.println s!"working {i}"
    if i == stop then
      IO.eprintln "exiting"
      IO.Process.exit code

def main (args : List String) : IO Unit := do
  if args[0]! == "force" then
    IO.println "buffered, lost"
    IO.eprintln "stderr, kept"
    IO.Process.forceExit args[1]!.toNat!.toUInt8
  IO.print "partial line "
  work args[1]!.toNat! args[2]!.toNat! args[3]!.toNat!.toUInt8
  IO.println "never printed"
