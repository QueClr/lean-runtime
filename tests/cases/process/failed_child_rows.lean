/-! The `Child` rows of a child that cannot start (a missing program, a `cwd` that cannot be
entered), with and without `setsid`. Natively the forked child exits at once with status 255:
`kill` signals the zombie and succeeds until it has been waited, `wait` gives 255 once, then
`wait`, `tryWait` and `kill` fail as for any reaped child. With `setsid`, the child of a missing
program has called `setsid()` before `execvp` failed, so `killpg` finds its group; the child of a
bad `cwd` failed at `chdir`, before `setsid()`, so no group has its id and `killpg` fails with
`ESRCH`. The sleep lets the child exit first, so `kill` never races with its exit. -/
def rows (label : String) (args : IO.Process.SpawnArgs) : IO Unit := do
  let c ← IO.Process.spawn { args with stdin := .null, stdout := .null, stderr := .piped }
  IO.println s!"{label}: stderr {repr (← c.stderr.readToEnd)}"
  IO.sleep 100
  try c.kill; IO.println s!"{label}: kill before wait ok" catch e => IO.println s!"{label}: kill before wait: {e}"
  IO.println s!"{label}: wait {← c.wait}"
  try c.kill; IO.println s!"{label}: kill after wait ok" catch e => IO.println s!"{label}: kill after wait: {e}"
  try IO.println s!"{label}: second wait {← c.wait}" catch e => IO.println s!"{label}: second wait: {e}"
  try IO.println s!"{label}: tryWait {← c.tryWait}" catch e => IO.println s!"{label}: tryWait: {e}"

def main (args : List String) : IO Unit := do
  let cmd := args.headD "no-such-program-xyz"
  rows "program" { cmd }
  rows "program setsid" { cmd, setsid := true }
  rows "cwd" { cmd := "true", cwd := some "no-such-dir" }
  rows "cwd setsid" { cmd := "true", cwd := some "no-such-dir", setsid := true }
  -- tryWait reaps the failed child as wait does
  let c ← IO.Process.spawn { cmd, stdin := .null, stdout := .null, stderr := .null }
  IO.sleep 100
  IO.println s!"tryWait first {← c.tryWait}"
  try IO.println s!"wait after tryWait {← c.wait}" catch e => IO.println s!"wait after tryWait: {e}"
