-- On a readWrite handle, a write after a partial read lands in glibc's
-- buffer at the read position, so part of it reaches the file before any
-- flush (finding A812, leanrs; repro ReWriteAfterRead). The case runs in a
-- fresh working directory.

def countW (s : String) : Nat := s.foldl (fun n c => if c == 'W' then n + 1 else n) 0

def main (args : List String) : IO Unit := do
  let size := args[0]!.toNat!
  let readN := args[1]!.toNat!
  let writeN := args[2]!.toNat!
  let f : System.FilePath := "re-war.txt"
  IO.FS.writeFile f ("".pushn 'x' size)
  let h ← IO.FS.Handle.mk f .readWrite
  let _ ← h.read readN.toUSize
  h.putStr ("".pushn 'W' writeN)
  IO.println s!"before flush {countW (← IO.FS.readFile f)}"
  h.flush
  IO.println s!"after flush {countW (← IO.FS.readFile f)}"
  IO.println s!"size {(← IO.FS.readFile f).length}"
