/-! `IO.Process.output` in `main` while a task prints every 50 ms: natively
the task's thread goes on while `main` waits for the child's output; on one
thread the wait must let the task run (sched-io). The twin calls the
runtime's own `output` (a translator may call it instead of translating
Lean's definition), which waits for both pipes on the calling context. The
child's sleep and the tick count come from argv. -/

def main (args : List String) : IO Unit := do
  let ticks := args[1]!.toNat!
  let ticker ← IO.asTask do
    for i in [0:ticks] do
      IO.sleep 50
      IO.println s!"ticker: {i}"
  let o ← IO.Process.output { cmd := "sh", args := #["-c", "sleep \"$1\"; echo out; echo err >&2; exit 2", "sh", args[0]!] }
  IO.println s!"output: {o.exitCode} {repr o.stdout} {repr o.stderr}"
  let _ ← IO.wait ticker
  IO.println "done"
