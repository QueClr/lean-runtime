-- Standard input read ahead in glibc's blocks: the program reads one line and
-- a few bytes; at exit `_IO_unbuffer_all` gives the read-ahead back to a
-- seekable stdin, so the next process on the same descriptor (`head` in .pipe)
-- reads from where the program stopped. The byte count comes from argv (from
-- lean2rr's tests/runtime/RtStdioStdinAhead.lean and RtStdioStdinFlush.lean).

def main (args : List String) : IO Unit := do
  let stdin ← IO.getStdin
  let l ← stdin.getLine
  let b ← stdin.read args.head!.toNat!.toUSize
  IO.println s!"got {repr l} and {b.size} bytes"
