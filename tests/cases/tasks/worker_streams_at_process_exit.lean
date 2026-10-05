-- As `worker_streams_closed_at_exit`, but `main` ends with
-- `IO.Process.exit 0`. Natively `exit` runs no thread finalizers, so the
-- worker's handle is still open, and glibc's exit flushes `stdout` first,
-- then the other open streams: `BA0` (also with 2 and 4 workers). Review
-- AR-33 (leanrs).

def main (args : List String) : IO Unit := do
  let n := args.length
  let t ← IO.asTask (do
    let h ← IO.FS.Handle.mk "/dev/stdout" .write
    discard <| IO.setStdout (IO.FS.Stream.ofHandle h)
    IO.print s!"A{n}")
  let _ ← IO.wait t
  IO.print "B"
  IO.Process.exit 0
