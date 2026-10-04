/-! The current standard streams are per thread (io.cpp, `MK_THREAD_LOCAL_GET` over
`g_stream_stdout` & co.): a task starts with the process's streams, whatever `main` has set,
and a stream a task sets is its own. -/
def text (r : IO.Ref IO.FS.Stream.Buffer) : IO String := do
  return String.fromUTF8! (← r.get).data

def main : IO Unit := do
  let out ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let err ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let oldOut ← IO.setStdout (IO.FS.Stream.ofBuffer out)
  let oldErr ← IO.setStderr (IO.FS.Stream.ofBuffer err)
  IO.println "main: captured"
  IO.eprintln "main: captured error"
  let t ← IO.asTask (do
    IO.println "task: to the real stdout"
    IO.eprintln "task: to the real stderr"
    let mine ← IO.mkRef ({} : IO.FS.Stream.Buffer)
    let prev ← IO.setStdout (IO.FS.Stream.ofBuffer mine)
    IO.println "task: own buffer"
    let _ ← IO.setStdout prev
    (← IO.getStdout).flush
    return (← text mine))
  let r ← IO.wait t
  IO.println "main: still captured"
  let _ ← IO.setStdout oldOut
  let _ ← IO.setStderr oldErr
  IO.println s!"task result: {repr r.toOption}"
  IO.println s!"main stdout buffer: {repr (← text out)}"
  IO.println s!"main stderr buffer: {repr (← text err)}"
  let t ← IO.asTask (prio := .dedicated) (do IO.println "dedicated task: real stdout"; (← IO.getStdout).flush)
  let _ ← IO.wait t
  IO.println "end"
