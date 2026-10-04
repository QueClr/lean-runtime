/-! `output_drain_exit`, ended by `IO.Process.forceExit 0` (`_Exit`) instead of `main`'s return (AR-6,
leanrs's review of fixes-1). `_Exit` flushes nothing and waits for nothing: the process ends at
once, its end of the pipe closes, and the child's late write fails with `EPIPE` (status 1;
`SIGPIPE` is ignored). Standard output is flushed first, or `_Exit` would lose its lines. The
`.pipe` waits for the file `marker`, then prints it. -/
def main : IO Unit := do
  try
    let o ← IO.Process.output { cmd := "sh", args := #["-c",
      "printf '\\377' >&2; exec 2>&-; sleep 1; echo late; echo \"child: write status $?\" > marker"] }
    IO.println s!"output: exit {o.exitCode}"
  catch e => IO.println s!"output failed: {e}"
  IO.println "main ends"
  (← IO.getStdout).flush
  IO.Process.forceExit 0
