/-! `output_drain_exit`, ended by `IO.Process.exit 0` instead of `main`'s return (AR-6, leanrs's
review of fixes-1; LB-29). `lean_io_exit` calls C's `exit`, which waits for no task. Natively its
flush of every `FILE` (`_IO_flush_all`) takes each stream's lock, and the standard-output reader
of `output` holds the lock of its pipe's stream in `fread` until the child's end of file: the
process ends only after the child's late write, which finds a reader (status 0). Correct (LB-29):
the process ends at once, and the late write fails with `EPIPE` (status 1). The `.pipe` waits for
the file `marker`, then prints it. -/
def main : IO Unit := do
  try
    let o ← IO.Process.output { cmd := "sh", args := #["-c",
      "printf '\\377' >&2; exec 2>&-; sleep 1; echo late; echo \"child: write status $?\" > marker"] }
    IO.println s!"output: exit {o.exitCode}"
  catch e => IO.println s!"output failed: {e}"
  IO.println "main ends"
  IO.Process.exit 0
