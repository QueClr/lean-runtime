/-! `IO.Process.output` of a child that writes more than a pipe holds to its
standard output before it writes its standard error. Lean's definition reads
standard output on a dedicated task while `main` reads standard error, so
`main` blocks in its read while the task drains the child's standard output
(sched-io: on one thread, `main`'s read must let the task run, or the child
blocks on its full pipe and the program hangs). The sizes come from argv. -/

def main (args : List String) : IO Unit := do
  let o ← IO.Process.output { cmd := "sh", args := #["-c",
    "yes a 2>/dev/null | head -c \"$1\"; yes b 2>/dev/null | head -c \"$2\" >&2; exit 3",
    "sh", args[0]!, args[1]!] }
  IO.println s!"code {o.exitCode} stdout {o.stdout.length} stderr {o.stderr.length}"
  IO.println s!"stdout {repr (o.stdout.take 4).toString} ... {repr (o.stdout.drop (o.stdout.length - 4)).toString}"
  IO.println s!"stderr {repr (o.stderr.take 4).toString} ... {repr (o.stderr.drop (o.stderr.length - 4)).toString}"
