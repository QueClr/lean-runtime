/-! An `append` handle's cursor starts at the end of the file (LB-46 of `docs/lean-bugs.md`;
file-system bug hunt HFS-01). `IO.FS.Mode.append` documents that "the read/write cursor is
positioned at the end of the file", and `Handle.truncate` "Truncates the handle to its read/write
cursor". Natively the cursor is at 0 after the open: Lean opens the file with `O_APPEND`, and
glibc's `fdopen(fd, "a")` then does not seek (it seeks only when it adds `O_APPEND` itself), so
a `truncate` right after the open empties the file and only the later write is left (`native`).
The correct outcome keeps the content. The second file is the contrast of LB-49 (not a bug,
followed as native): a byte written but not flushed, then `truncate`, which counts that byte from
the end of the file, so the flush appends it after a NUL. The contents come from argv. -/
def main (args : List String) : IO Unit := do
  let (keep, more, pending) := (args[0]!, args[1]!, args[2]!)
  IO.FS.writeFile "a.txt" keep
  let h ← IO.FS.Handle.mk "a.txt" .append
  h.truncate
  h.putStr more
  h.flush
  IO.println s!"truncate after open: {repr (← IO.FS.readFile "a.txt")}"
  IO.FS.writeFile "b.txt" keep
  let h2 ← IO.FS.Handle.mk "b.txt" .append
  h2.putStr pending
  h2.truncate
  h2.flush
  IO.println s!"truncate with a pending byte: {repr (← IO.FS.readFile "b.txt")}"
