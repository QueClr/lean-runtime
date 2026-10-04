/-! Spawns with the parent's descriptors exhausted (the `.pipe` runs it under `ulimit -n 64`): Lean's
forked child needs no new descriptor to fail at `execvp` or `chdir`, or to enter a relative `cwd`,
so the spawns succeed: a missing program and a missing directory exit with 255 and their
messages, `true` in `.` or `/` with 0 (leanrs review LRIO2-F3, native/E.lean). The program keeps
its handles open to the end. -/
partial def exhaust (acc : Array IO.FS.Handle) : IO (Array IO.FS.Handle) := do
  match ← (IO.FS.Handle.mk "/dev/null" .read).toBaseIO with
  | .ok h => exhaust (acc.push h)
  | .error _ => pure acc

def attempt (label : String) (args : IO.Process.SpawnArgs) : IO Unit := do
  try
    let c ← IO.Process.spawn args
    IO.println s!"{label}: exit {← c.wait}"
  catch e => IO.println s!"{label}: spawn failed: {e}"
  (← IO.getStdout).flush

def main : IO Unit := do
  let hs ← exhaust #[]
  attempt "missing program" { cmd := "no-such-program-xyz" }
  attempt "relative cwd" { cmd := "true", cwd := some "." }
  attempt "absolute cwd" { cmd := "true", cwd := some "/" }
  attempt "missing dir" { cmd := "true", cwd := some "no-such-dir" }
  IO.println s!"kept {decide (hs.size > 0)}"
