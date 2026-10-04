-- glibc's stdout write rules, seen through where the unbuffered stderr lines
-- land (stdout and stderr merged into one pipe, block size 4096; .pipe). The
-- very first write finds no buffer: whole blocks go straight to the
-- descriptor. Later writes fill the buffer first. Line lengths from argv
-- (from lean2rr's tests/runtime/RtStdoutBlock.lean).

def line (c : Char) (n : Nat) : String := String.ofList (List.replicate (n - 1) c) ++ "\n"

def main (args : List String) : IO Unit := do
  let mut i := 0
  for (a, c) in args.zip ['a', 'b', 'c', 'd', 'e', 'f'] do
    i := i + 1
    IO.print (line c a.toNat!)
    IO.eprintln s!"STDERR {i}"
  IO.println "end"
