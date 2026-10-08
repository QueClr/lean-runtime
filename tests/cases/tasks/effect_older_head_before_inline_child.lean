-- One worker (`LEAN_NUM_THREADS=1`; the other translator's second review of
-- lean-runtime's fixes-19). `main` queues `h`, which prints "H", then `a` at
-- `Task.Priority.max`, which queues `t` (prints "T", at the default
-- priority, as `h`) and ends. `main` waits for `a` (lean-runtime's
-- single-thread scheduler runs it there, on `main`'s stack), prints "main"
-- and waits for `h`; `t` runs by the exit. Natively `h` is queued before
-- `t`, at the same priority, so the one worker takes `h` first: "H" comes
-- before "T" in every schedule. An effect point at "main" that lets `t`
-- pass, as queued while the worker that `a`'s end freed was held, and
-- passes over the older `h`, prints "T" first.
def main : IO Unit := do
  let h ← IO.asTask (IO.println "H")
  let a ← IO.asTask (prio := .max) (do
      let _ ← IO.asTask (IO.println "T")
      pure ())
  let _ ← IO.wait a
  IO.println "main"
  let _ ← IO.wait h
