/-! A task's current standard streams are its thread's (`IO.setStdout`
replaces "the stdout of the current thread"; io.cpp, `MK_THREAD_LOCAL_GET`):
- task A sets its stdout to a buffer and does not restore it; while A
  sleeps, `main` prints to its own stdout;
- task B, which runs after A on the same pool worker, prints into A's
  buffer: a worker keeps its streams from one task to the next;
- task C, a dedicated task, has a thread of its own, with the process's
  streams. (Review RT2-L-01 and AR-24 of lean-runtime.) -/
def main : IO Unit := do
  let buf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let a ← IO.asTask (do
    let _ ← IO.setStdout (IO.FS.Stream.ofBuffer buf)
    IO.println "A: before its sleep"
    IO.sleep 200
    IO.println "A: after its sleep")
  IO.sleep 100
  IO.println "main: while A sleeps"
  let _ ← IO.wait a
  let b ← IO.asTask (IO.println "B: after A, on A's worker")
  let _ ← IO.wait b
  let c ← IO.asTask (prio := .dedicated) (IO.println "C: a thread of its own")
  let _ ← IO.wait c
  IO.println s!"A's buffer: {repr (String.fromUTF8! (← buf.get).data)}"
