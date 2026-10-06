/-! `IO.Process.output` with an input larger than a pipe holds while a task runs (LB-40 of
`docs/lean-bugs.md`; io bug hunt HIO-01): the child sleeps, then copies its input (`cat`). Lean
writes all of the input before it reads anything, so native waits for good once `cat` fills its
standard output's pipe (`native`: the timeout; the ticker's lines stay in the buffer). The correct
outcome: the ticker runs while `output` waits for room in the input's pipe, then all of the input
comes back. The sizes and the delay come from argv. -/
def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let ticks := args[1]!.toNat!
  let ticker ← IO.asTask do
    for i in [0:ticks] do
      IO.sleep 50
      IO.println s!"ticker: {i}"
  let input := "".pushn 'a' n
  let o ← IO.Process.output { cmd := "sh", args := #["-c", "sleep \"$1\"; cat", "sh", args[2]!] } (some input)
  IO.println s!"output: {o.exitCode}, {o.stdout.length} bytes back, same {decide (o.stdout = input)}"
  let _ ← IO.wait ticker
  IO.println "done"
