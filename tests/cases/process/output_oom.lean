/-! `IO.Process.output` whose child writes without end on one pipe and has closed the other
(AR-5, from leanrs's adopt-io-2). The case runs the program twice under `ulimit -v 3000000`,
with `LEAN_STACK_SIZE_KB=8192` so that native's threads fit: once with argument `stdout` (the
child writes standard output, which the dedicated task reads, and has closed standard error),
once with `stderr` (the child writes standard error, which `main` reads, and has no standard
output). Each time the `ByteArray` that `readToEnd` grows cannot be allocated: the program
prints `INTERNAL PANIC: out of memory` and exits with status 1, and `exit` flushes the line
printed before. The child is not waited; it gets `EPIPE` when the process has ended. When the
child keeps the other pipe open without writing to it (`yes` alone), native hangs in `exit`
instead: `exit` flushes every `FILE` under its lock, and the other thread holds the lock of the
other pipe's stream in a blocked `fread`. -/
def main (args : List String) : IO UInt32 := do
  let which := args.headD ""
  IO.println s!"before {which}"
  let script := if which == "stdout" then "exec yes 2>&-" else "exec yes >&2"
  let o ← IO.Process.output { cmd := "sh", args := #["-c", script] }
  IO.println s!"after {which}: exit {o.exitCode}, {o.stdout.length} and {o.stderr.length} bytes"
  return 0
