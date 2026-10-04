-- `Handle.lock` (`flock(fileno(fp))`) takes no `FILE` lock natively, so it
-- does not wait for a task blocked in a read on the same handle: `main` locks
-- while the task waits in `getLine` for a line that .pipe sends half a
-- second later (review RIO1-01, the blocked-read variant). The path comes
-- from argv.

def main (args : List String) : IO Unit := do
  let h ← IO.FS.Handle.mk args[0]! .read
  let t ← IO.asTask (prio := .dedicated) do
    let l ← h.getLine
    IO.println s!"task: got {repr l}"
  IO.sleep args[1]!.toNat!.toUInt32
  h.lock (exclusive := true)
  IO.println "main: locked while the task reads"
  let _ ← IO.wait t
  h.unlock
  IO.println "done"
