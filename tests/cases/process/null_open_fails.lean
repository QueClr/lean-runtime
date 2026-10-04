/-! A `null` stream when `/dev/null` cannot be opened (process.cpp, `spawn`: the forked child's
`open("/dev/null")` result is unchecked, `dup2(-1, n)` fails, and the program keeps the
parent's descriptor n). Run under `ulimit -n 64` with `line1`, `line2` on standard input: the
parent opens handles until `EMFILE`, then spawns. Native: the child writes to the parent's
standard output and reads the parent's first line. Correct (LB-17, both translators): the spawn fails with
`EMFILE`, and the parent still reads `line1`. -/
partial def exhaust (acc : Array IO.FS.Handle) : IO (Array IO.FS.Handle) := do
  match ← (IO.FS.Handle.mk "/dev/null" .read).toBaseIO with
  | .ok h => exhaust (acc.push h)
  | .error _ => pure acc

def main : IO Unit := do
  let hs ← exhaust #[]
  IO.println "stdout null:"
  (← IO.getStdout).flush
  try
    let c ← IO.Process.spawn { cmd := "sh", args := #["-c", "echo '  child: written to the parent'"], stdout := .null }
    IO.println s!"  exit {← c.wait}"
  catch e => IO.println s!"  spawn failed: {e}"
  IO.println "stdin null:"
  (← IO.getStdout).flush
  try
    let c ← IO.Process.spawn { cmd := "sh", args := #["-c", "if read x; then echo \"  child: read $x\"; else echo '  child: no input'; fi"], stdin := .null }
    IO.println s!"  exit {← c.wait}"
  catch e => IO.println s!"  spawn failed: {e}"
  IO.println s!"parent reads: {(← (← IO.getStdin).getLine).trimAscii}"
  -- keeps the handles open until here
  IO.println s!"handles kept open: {decide (hs.size > 0)}"
