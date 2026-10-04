-- `result_bang_dropped` under `LEAN_ABORT_ON_PANIC`: the panic's line, then
-- an abort (status 134). The line goes to the process's standard error
-- although `IO.setStderr` has replaced Lean's (`force_stderr`), after C's
-- `stdout` is flushed ("before"), though an abort flushes nothing.

def main (_args : List String) : IO Unit := do
  IO.println "before"
  let buf ← IO.mkRef {}
  let _ ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
  let p ← IO.Promise.new (α := Nat)
  let t := p.result!
  IO.eprintln s!"not reached: {← IO.hasFinished t}"
