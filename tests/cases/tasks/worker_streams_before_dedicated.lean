-- A pool task makes `cat`'s standard input its stdout and prints a line
-- there (buffered: the handle is a pipe), then leaves it set; a dedicated
-- task waits for `cat`. Natively the task manager's finalization joins the
-- standard workers first, and their thread finalizers drop their current
-- streams, so `cat`'s input is closed and `cat` prints the line and ends;
-- only then does it wait for the dedicated threads (`~task_manager`,
-- `object.cpp` 972-988): `main done`, `via cat 0`, `cat exited 0` (also
-- with 2 and 4 workers). Review AR-34 (leanrs) of lean-runtime: the
-- workers' streams were dropped only after the dedicated tasks had ended,
-- so the dedicated task waited for `cat` forever.

def main (args : List String) : IO Unit := do
  let child ← IO.Process.spawn { cmd := "cat", stdin := .piped, stdout := .inherit }
  let (stdin, child) ← child.takeStdin
  let t ← IO.asTask (do
    discard <| IO.setStdout (IO.FS.Stream.ofHandle stdin)
    IO.println s!"via cat {args.length}")
  let _ ← IO.wait t
  let _d ← IO.asTask (prio := .dedicated) (do
    let c ← child.wait
    IO.eprintln s!"cat exited {c}")
  IO.eprintln "main done"
