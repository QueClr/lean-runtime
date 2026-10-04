-- `result_bang_dropped`, the promise dropped in a dedicated task: that
-- thread prints the panic (flushing C's `stdout`: "before") and sleeps
-- forever, while `main` and another task go on. `main` returns, and the
-- exit waits for the sleeping thread forever (`lean_finalize_task_manager`),
-- so "main done", buffered on `stdout`, is never written.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  IO.println "before"
  let t ← IO.asTask (prio := .dedicated) do
    let p ← IO.Promise.new (α := Nat)
    let t := p.result!
    IO.eprintln s!"not reached: {← IO.hasFinished t}"
  IO.sleep ms.toUInt32
  IO.eprintln s!"main: dropping task finished: {← IO.hasFinished t}"
  let o ← IO.asTask (IO.eprintln "another task runs")
  let _ ← IO.wait o
  IO.println "main done"
  IO.eprintln "main returns"
