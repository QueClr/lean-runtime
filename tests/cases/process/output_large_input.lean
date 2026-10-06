/-! `IO.Process.output` with an input larger than a pipe holds (LB-40 of `docs/lean-bugs.md`; io
bug hunt HIO-01). `cat` writes its standard output while it reads its input, so it fills that
pipe before it has read all of it. Lean's `output` writes and flushes all of the input before it
reads anything (Init/System/IO.lean 1544-1557), so native waits for good (`native`: the timeout,
nothing printed). The correct outcome gives all of the input back. The size comes from argv. -/
def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let input := "".pushn 'a' n
  let out ← IO.Process.output { cmd := "cat" } (some input)
  IO.println s!"cat {n}: exit {out.exitCode}, {out.stdout.length} bytes back, same {decide (out.stdout = input)}, stderr {repr out.stderr}"
