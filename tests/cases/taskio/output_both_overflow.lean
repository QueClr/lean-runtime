/-! `IO.Process.output` of a child that overflows both of its pipes, in
turns: each round writes more than a pipe holds to standard output, then to
standard error. The task reading standard output and `main` reading standard
error must each go on while the other waits (sched-io). The round count and
size come from argv. -/

def main (args : List String) : IO Unit := do
  let o ← IO.Process.output { cmd := "sh", args := #["-c",
    "i=0; while [ $i -lt \"$1\" ]; do yes o 2>/dev/null | head -c \"$2\"; yes e 2>/dev/null | head -c \"$2\" >&2; i=$((i+1)); done",
    "sh", args[0]!, args[1]!] }
  IO.println s!"code {o.exitCode} stdout {o.stdout.length} stderr {o.stderr.length}"
  IO.println s!"stdout all o: {o.stdout.all (fun c => c == 'o' || c == '\n')} stderr all e: {o.stderr.all (fun c => c == 'e' || c == '\n')}"
