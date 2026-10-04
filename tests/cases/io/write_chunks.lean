-- The write(2) chunks of stdout and of a file handle, and a readWrite
-- handle's interplay of reads, writes and a rewind, as glibc's stdio makes
-- them (.pipe hashes the merged output and both files). Each argument is one
-- write: `+`-joined parts, a part being `/` (a newline) or a character and a
-- count (from lean2rr's tests/runtime/RtStdioChunks.lean).

def rep (c : Char) (n : Nat) : String := String.ofList (List.replicate n c)

def part (p : String) : String :=
  if p == "/" then "\n" else rep p.front (p.drop 1).copy.toNat!

def main (args : List String) : IO Unit := do
  let seq := args.map fun a => String.join ((a.splitOn "+").map part)
  for s in seq do
    IO.print s
    IO.eprint "|"
  let h ← IO.FS.Handle.mk "c1.txt" .write
  for s in seq do
    h.putStr s
  h.flush
  IO.FS.writeFile "c2.txt" (rep 'z' 20000)
  let r ← IO.FS.Handle.mk "c2.txt" .readWrite
  let _ ← r.getLine
  r.putStr (rep 'A' 10)
  let _ ← r.read 100
  r.putStr (rep 'B' 5000)
  let _ ← r.read 3000
  r.putStr (rep 'C' 1500)
  r.rewind
  r.putStr (rep 'D' 4096)
  let _ ← r.read 1
  r.putStr "E"
  r.flush
  IO.println ""
