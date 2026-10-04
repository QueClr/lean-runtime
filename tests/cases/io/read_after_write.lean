-- Output directly followed by a large read on one handle (LB-02 in
-- docs/lean-bugs.md; leanrs DV20 (a)). Lean's `Handle.read` is glibc's
-- `fread`, and a read of at least one buffer right after output makes glibc
-- discard the pending output (C11 7.21.5.3p7 leaves output followed by input
-- without a flush or a seek undefined): the written bytes never reach the
-- file. The expected output is the correct one (pending output written
-- first, then the read from the cursor, EBADF on a write-only handle);
-- native's is in the case's `native` field.
-- - F: write-only handle, `putStr` then `read 5000` (fails);
-- - A: append handle, the same;
-- - G: read-write handle on "0123456789", `putStr` then `read 5000`;
-- - S: the same with `read 4` (glibc flushes for a small read: no loss);
-- - O: standard output (a pipe here), `print` then `read 5000` (fails).
-- The written text comes from the command line.

def showErr (act : IO α) (fmt : α → String) : IO String := do
  try return fmt (← act) catch e => return s!"error: {e}"

def main (args : List String) : IO Unit := do
  let t := args[0]!
  let w ← IO.FS.Handle.mk "f.txt" .write
  w.putStr t
  let r ← showErr (w.read 5000) fun b => s!"read {b.size}"
  w.flush
  IO.println s!"F: {r}; contents {repr (← IO.FS.readFile "f.txt")}"
  IO.FS.writeFile "a.txt" "0123"
  let a ← IO.FS.Handle.mk "a.txt" .append
  a.putStr t
  let r ← showErr (a.read 5000) fun b => s!"read {b.size}"
  a.flush
  IO.println s!"A: {r}; contents {repr (← IO.FS.readFile "a.txt")}"
  IO.FS.writeFile "g.txt" "0123456789"
  let h ← IO.FS.Handle.mk "g.txt" .readWrite
  h.putStr t
  let b ← h.read 5000
  h.flush
  IO.println s!"G: read {repr (String.fromUTF8! b)}; contents {repr (← IO.FS.readFile "g.txt")}"
  IO.FS.writeFile "s.txt" "0123456789"
  let h ← IO.FS.Handle.mk "s.txt" .readWrite
  h.putStr t
  let b ← h.read 4
  h.flush
  IO.println s!"S: read {repr (String.fromUTF8! b)}; contents {repr (← IO.FS.readFile "s.txt")}"
  let out ← IO.getStdout
  out.flush
  IO.print s!"O: {t} printed; "
  let r ← showErr (out.read 5000) fun b => s!"read {b.size}"
  IO.println s!"{r}"
