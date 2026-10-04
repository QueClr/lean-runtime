-- Standard streams closed at startup (.pipe runs every combination): the
-- descriptors native Lean opens before `main` take the free numbers, the
-- epoll descriptor first, then the io_uring rings, so reading or writing a
-- closed stream reaches them and fails with EINVAL (a direct read of a
-- buffer or more reads the descriptor itself), as natively (LB-10, followed;
-- finding A821). The results go to the file named by the first argument.

def tryIO (act : IO String) : IO String := do
  try act catch e => return s!"error: {e}"

def main (args : List String) : IO Unit := do
  let report ← IO.FS.Handle.mk args[0]! .write
  let n := args[1]!.toNat!
  let mut fds : Array Nat := #[]
  for e in ← System.FilePath.readDir "/proc/self/fd" do
    if let some n := e.fileName.toNat? then fds := fds.push n
  let r1 ← tryIO do
    let l ← (← IO.getStdin).getLine
    return s!"stdin getLine: {repr l}"
  let r2 ← tryIO do
    let b ← (← IO.getStdin).read 5
    return s!"stdin read: {b.size}"
  let r3 ← tryIO do
    IO.print "out\n"
    (← IO.getStdout).flush
    return "stdout: ok"
  let r4 ← tryIO do
    let e ← IO.getStderr
    e.putStr "err\n"
    e.flush
    return "stderr: ok"
  let r5 ← tryIO do
    let b ← (← IO.getStdout).read 5
    return s!"stdout read: {b.size}"
  let r6 ← tryIO do
    let b ← (← IO.getStdout).read n.toUSize
    return s!"stdout read {n}: {b.size}"
  let r7 ← tryIO do
    let b ← (← IO.getStderr).read n.toUSize
    return s!"stderr read {n}: {b.size}"
  report.putStr s!"fds {fds.qsort (· < ·)}\n{r1}\n{r2}\n{r3}\n{r4}\n{r5}\n{r6}\n{r7}\n"
