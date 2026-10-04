/-! `IO.Process.output` fails on standard error that is not UTF-8 while the child writes standard
output without end (`yes`), under `ulimit -v 3000000` (review RFX1-04 of fixes-1; AR-6). The
dedicated standard-output task goes on after `output` has thrown, and `main` returns; the task's
`readToEnd` grows its `ByteArray` until the allocation fails: `INTERNAL PANIC: out of memory`,
exit 1, from the task's thread, which flushes `main`'s two lines (`main` holds no stream: it waits
in `lean_finalize_task_manager`). -/
def main : IO Unit := do
  try
    let o ← IO.Process.output { cmd := "sh", args := #["-c", "printf '\\377' >&2; exec 2>&-; exec yes"] }
    IO.println s!"output: exit {o.exitCode}"
  catch e => IO.println s!"output failed: {e}"
  IO.println "main ends"
