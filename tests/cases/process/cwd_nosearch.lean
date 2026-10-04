/-! A process that may not search its own working directory spawns children with a `cwd`
(leanrs DV15 (d)). Natively the forked child's `chdir` resolves a relative `cwd` against the
unsearchable directory and fails (the child's message, status 255), and enters an absolute one;
a child without `cwd` runs. The parent's working directory never changes. -/
def mode (r w x : Bool) : IO.FileRight := { user := { read := r, write := w, execution := x } }

def report (label : String) (o : IO.Process.Output) : IO Unit := do
  IO.println s!"{label}: exit {o.exitCode} out {repr o.stdout} err {repr o.stderr}"
  (← IO.getStdout).flush

def attempt (label : String) (args : IO.Process.SpawnArgs) : IO Unit := do
  try report label (← IO.Process.output args) catch e => IO.println s!"{label}: error {e}"

def main : IO Unit := do
  let top ← IO.currentDir
  let here := top / "here"
  IO.FS.createDirAll (here / "sub")
  IO.Process.setCurrentDir here
  IO.setAccessRights here (mode true true false)
  attempt "relative" { cmd := "pwd", cwd := some "sub" }
  attempt "absolute" { cmd := "pwd", cwd := some "/" }
  attempt "absolute missing program" { cmd := "no-such-program-xyz", cwd := some "/" }
  attempt "none" { cmd := "sh", args := #["-c", "echo ran"] }
  IO.setAccessRights here (mode true true true)
  IO.println s!"cwd unchanged: {(← IO.currentDir) == here}"
  IO.Process.setCurrentDir top
  IO.FS.removeDirAll here
