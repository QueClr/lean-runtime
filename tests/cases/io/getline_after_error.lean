/-! `getLine` after a failed write on the same handle (LB-41 of `docs/lean-bugs.md`; io bug hunt
HIO-02). The write sets the stream's error indicator (`EBADF`: the handle is read-only). Lean's
`getLine` tests that indicator after it reads (io.cpp 659), and only end of file clears it, so
natively every later `getLine` reads its line and fails, the line lost, also at end of file
(`native`). The correct outcome: each `getLine` reports only its own error, so the lines come
back. The lines come from argv. -/
def main (args : List String) : IO Unit := do
  IO.FS.writeFile "f.txt" (String.join (args.map (· ++ "\n")))
  let h ← IO.FS.Handle.mk "f.txt" .read
  IO.println s!"getLine: {repr (← h.getLine)}"
  try h.putStr "x" catch e => IO.println s!"putStr: {e}"
  for _ in [0:args.length] do
    try IO.println s!"getLine: {repr (← h.getLine)}"
    catch e => IO.println s!"getLine: error: {e}"
