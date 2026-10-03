-- An IO task is kept alive until it has run, and Lean's exit waits for it,
-- so a runaway IO task that main never awaits keeps the process alive.
-- Main's buffered stdout line never appears on a pipe, because the exit joins
-- the tasks before it flushes (leanrs exit probe 1, PExitIO; refinement A).

partial def spinIO (r : IO.Ref UInt64) : IO Unit := do
  r.modify (· + 1)
  spinIO r

def main (args : List String) : IO Unit := do
  let r ← IO.mkRef args.length.toUInt64
  let _t ← IO.asTask (spinIO r)
  IO.println "main done"
