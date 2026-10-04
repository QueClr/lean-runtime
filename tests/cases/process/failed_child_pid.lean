/-! A child that cannot start has a process id natively: Lean's `fork` succeeds, and the forked
child writes its message and exits with status 255 (process.cpp, `spawn`). `posix_spawn` reports
the failure to the parent instead, so the shared runtime starts a stand-in (`/bin/sh -c 'read x
<&3; printf %s "$1" >&2; exit 255'`, which waits for a release, writes the message and exits) to
give it a real pid. The other rows of such a child are `failed_child_rows`. -/
def main (args : List String) : IO Unit := do
  let cmd := args.headD "no-such-program-xyz"
  let c ← IO.Process.spawn { cmd, stdin := .null, stdout := .null, stderr := .null }
  IO.println s!"program: pid positive {decide (c.pid > 0)}"
  IO.println s!"program: wait {← c.wait}"
  let c ← IO.Process.spawn { cmd := "true", cwd := some "no-such-dir", stdin := .null, stdout := .null, stderr := .null }
  IO.println s!"cwd: pid positive {decide (c.pid > 0)}"
  IO.println s!"cwd: wait {← c.wait}"
