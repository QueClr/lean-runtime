/-! `IO.Process.output` of `yes` under `ulimit -v 3000000` (LB-29; AR-5). The child floods its
standard output and never writes or closes its standard error. The `ByteArray` that the
standard-output task's `readToEnd` grows cannot be allocated: `INTERNAL PANIC: out of memory`.
Natively the panic's `exit(1)` then waits in glibc's `_IO_flush_all` for the lock of the
standard-error pipe's stream, which `main`'s `readToEnd` holds in a blocked `fread`; the child,
whose standard output nobody reads any more, blocks too: the process never ends (0% CPU).
Correct (both translators): the panic ends the process with status 1, the line printed before
flushed. `output_oom` is the same with the other pipe closed, where native ends. The command
comes from argv. -/
def main (args : List String) : IO UInt32 := do
  IO.println "before"
  let o ← IO.Process.output { cmd := args.headD "yes" }
  IO.println s!"after {o.exitCode} {o.stdout.length}"
  return 0
