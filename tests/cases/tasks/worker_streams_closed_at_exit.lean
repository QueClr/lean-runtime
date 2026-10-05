-- A pool task opens `/dev/stdout` as a handle, makes it its stdout, prints
-- `A0` there (buffered: the handle is a pipe) and does not restore it;
-- `main` waits for it, then prints `B` on its own stdout. Natively the task
-- manager's finalization joins the workers, and each worker's thread
-- finalizers drop its current streams (`lean_finalize_thread`,
-- `thread.cpp` 58-61; `MK_THREAD_LOCAL_GET`): the handle is closed, so
-- `A0` is written, before `main`'s stdout is flushed at the exit: `A0B`
-- (also with 2 and 4 workers). Review AR-33 (leanrs): lean-runtime kept the
-- emulated worker's streams to the end, so the exit's flush wrote `B`
-- first.

def main (args : List String) : IO Unit := do
  let n := args.length
  let t ← IO.asTask (do
    let h ← IO.FS.Handle.mk "/dev/stdout" .write
    discard <| IO.setStdout (IO.FS.Stream.ofHandle h)
    IO.print s!"A{n}")
  let _ ← IO.wait t
  IO.print "B"
