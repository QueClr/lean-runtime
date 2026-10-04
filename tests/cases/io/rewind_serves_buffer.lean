-- LB-09 (docs/lean-bugs.md, followed as native): `Handle.rewind` is
-- `fseek(fp, 0, SEEK_SET)`. The first rewind after the first read (offset
-- unknown) reads the file again; a second one lands inside glibc's buffer and
-- serves the buffered bytes, though another writer has rewritten the file.
-- The texts come from argv.

def main (args : List String) : IO Unit := do
  let f : System.FilePath := "s.txt"
  IO.FS.writeFile f args[0]!
  let h ← IO.FS.Handle.mk f .read
  IO.println s!"first: {repr (← h.getLine)}"
  IO.FS.writeFile f args[1]!
  h.rewind
  IO.println s!"after rewind 1: {repr (← h.getLine)}"
  IO.FS.writeFile f args[2]!
  h.rewind
  IO.println s!"after rewind 2: {repr (← h.getLine)}"
  IO.println s!"rest: {repr (← h.getLine)}"
