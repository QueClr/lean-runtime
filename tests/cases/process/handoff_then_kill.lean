/-! `handoff_then_resolve` with `Child.kill` in place of the resolution (leanrs's re-check of
lean-runtime fixes-1, AR-8). The program writes N bytes (from argv) to the stdin of
`sh -c 'sleep 1; cat > out'` and drops the handle while the pipe is full: natively the drop's
`fclose` blocks until `cat` has read, so `cat` has started and `out` exists when `kill` stops the
shell; `cat` then reads to the end and `out` gets all N. In a translator whose drop hands the last
bytes to a writer thread, the dropping context waits for that writer before it kills the child
(an io call with an effect outside the process). The `.pipe` looks at `out` two seconds later.
After `cat`, the child stays alive (`exec sleep 30`), so the kill always ends it (137), however
late it comes on a loaded host (the first version's shell could exit 0 before it); its stdout and
stderr are null, so nothing waits on them. -/
def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  -- a program with tasks
  let _t ← IO.asTask (pure ())
  let child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", "sleep 1; cat > out; exec sleep 30"], stdin := .piped,
      stdout := .null, stderr := .null }
  let (stdin, child) ← child.takeStdin
  stdin.putStr ("".pushn 'a' n)
  -- `stdin`'s last use: its finalizer closes it here, before the kill
  child.kill
  IO.println s!"killed, exit {← child.wait}"
