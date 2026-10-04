-- `IO.Process.exit` while a task waits in `b.lock` (`flock`, no `FILE` lock
-- natively): the exit flushes `b`'s pending output and ends the process; the
-- task never gets the lock. The file name and the wait come from argv; .pipe
-- prints the file afterwards (review RIO1-01, the reviewer's LockExit).

def main (args : List String) : IO Unit := do
  let f := args[0]!
  IO.FS.writeFile f ""
  let a ← IO.FS.Handle.mk f .readWrite
  let b ← IO.FS.Handle.mk f .readWrite
  a.lock (exclusive := true)
  let _t ← IO.asTask (prio := .dedicated) do
    b.lock (exclusive := true)
    IO.println "task: b locked"
  IO.sleep args[1]!.toNat!.toUInt32
  b.putStr "y"
  IO.println "main: exiting with the task still in b.lock"
  IO.Process.exit 0
  a.unlock
