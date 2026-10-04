/-! `IO.Process.exit` while a task is blocked reading a pipe (LB-29; the lean2rr-side judge's
`ExitWhileReading.lean`). A dedicated task reads the standard output of a child `sleep S` (S from
argv); `main` waits 300 ms, prints a line, flushes, and calls `IO.Process.exit 3`. Natively C's
`exit` flushes every `FILE` under its lock (glibc 2.39's `_IO_flush_all`), and the task's `fread`
holds the pipe's stream until the child ends, so the process ends only after S seconds (here
past the case's 3-second bound). Correct (both translators): the process ends at once with
status 3. The child's standard input and error are `null`, so that it holds none of the runner's
pipes. -/
def main (args : List String) : IO Unit := do
  let _t ← IO.asTask (prio := .dedicated) do
    let child ← IO.Process.spawn
      { cmd := "sleep", args := #[args.headD "10"], stdin := .null, stdout := .piped, stderr := .null }
    let s ← child.stdout.readToEnd
    IO.println s!"read {s.length}"
  IO.sleep 300
  IO.println "exiting with 3"
  (← IO.getStdout).flush
  IO.Process.exit 3
