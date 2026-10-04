-- `result_bang_dropped` with Lean's stderr set to a buffer first and no
-- `LEAN_ABORT_ON_PANIC`: the panic of `IO.Option.getOrBlock!` is a
-- `lean_panic` with `force_stderr`, so its line goes to the process's
-- standard error (`std::cerr`), not to the buffer, and the process hangs.
-- (Under `LEAN_ABORT_ON_PANIC` every Lean panic goes to `std::cerr`, so
-- `result_bang_dropped_abort` cannot tell the two streams apart.)

def main (_args : List String) : IO Unit := do
  IO.println "before"
  let buf ← IO.mkRef {}
  let _ ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
  let p ← IO.Promise.new (α := Nat)
  let t := p.result!
  IO.eprintln s!"not reached: {← IO.hasFinished t}"
