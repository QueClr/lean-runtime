/-! `IO.Process.output` fails on standard error that is not UTF-8 while the child still writes
standard output (AR-6, from leanrs). Lean's `output` reads standard output on a dedicated task,
which goes on after `output` has thrown; `lean_finalize_task_manager`, after `main` returns,
waits for the dedicated tasks, so the process ends only at the child's end of file. The child
closes standard error, sleeps one second (by then `main` has returned), writes a line to standard
output and records the write's status in the file `marker`, which the case prints after the
program: 0, since the pipe is still read. A process that ended at once would have closed the
pipe: the write would fail with `EPIPE` (status 1; `SIGPIPE` is ignored) and `marker` would come
too late or not at all. -/
def main : IO Unit := do
  try
    let o ← IO.Process.output { cmd := "sh", args := #["-c",
      "printf '\\377' >&2; exec 2>&-; sleep 1; echo late; echo \"child: write status $?\" > marker"] }
    IO.println s!"output: exit {o.exitCode}"
  catch e => IO.println s!"output failed: {e}"
  IO.println "main ends"
