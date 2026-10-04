/-! A task writes N bytes (from argv) to a child's stdin, drops the handle, then resolves a
promise; `main` waits for the promise and calls `IO.Process.exit 0` (leanrs's re-check of
lean-runtime fixes-1, AR-8). The child (`sleep 1; cat > out`) reads only after a second, so the
drop's `fclose` blocks the task until then: natively nothing the task does after the drop (here
the resolution) is seen before the child has read every byte, and `out` gets all N. In a
translator whose drop hands the last bytes to a writer thread, the task must wait for that
writer before it resolves the promise. The `.pipe` waits for the child's marker, then prints the
count. -/
def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let p ← IO.Promise.new
  let _a ← IO.asTask (prio := .dedicated) do
    let child ← IO.Process.spawn
      { cmd := "sh", args := #["-c", "sleep 1; cat > out; echo done > marker"], stdin := .piped }
    let (stdin, _child) ← child.takeStdin
    stdin.putStr ("".pushn 'a' n)
    -- `stdin`'s last use: its finalizer closes it here, before the resolution
    p.resolve ()
  IO.wait p.result!
  IO.println "main: resolved, exiting"
  (← IO.getStdout).flush
  IO.Process.exit 0
