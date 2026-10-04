-- `Handle.lock` is `flock(fileno(fp))`, which takes no `FILE` lock natively:
-- while a task waits in `b.lock`, `main` writes and flushes through the same
-- handle `b`, then releases the lock that `a` holds. The file name and the
-- wait come from argv (review RIO1-01, the reviewer's LockBlocked).

def main (args : List String) : IO Unit := do
  let f := args[0]!
  IO.FS.writeFile f ""
  let a ← IO.FS.Handle.mk f .readWrite
  let b ← IO.FS.Handle.mk f .readWrite
  a.lock (exclusive := true)
  let t ← IO.asTask (prio := .dedicated) do
    b.lock (exclusive := true)
    IO.println "task: b locked"
  IO.sleep args[1]!.toNat!.toUInt32
  b.putStr "x"
  b.flush
  IO.println s!"main: wrote through b while the task waits in b.lock"
  a.unlock
  let _ ← IO.wait t
  IO.println s!"done; file {repr (← IO.FS.readFile f)}"
