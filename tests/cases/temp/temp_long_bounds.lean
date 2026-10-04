/-! The neighbours of LB-16's window, where native gives an `IO.Error`: a temporary directory of
4082 bytes (the system's `ENAMETOOLONG`, error 36) and of 4096 bytes (libuv's `ENOBUFS`, error
105), for `createTempFile` and `createTempDir`. -/
def main (args : List String) : IO Unit := do
  IO.println "before"
  (← IO.getStdout).flush
  try
    if args == ["dir"] then
      let d ← IO.FS.createTempDir
      IO.println s!"created dir ({d.toString.length} bytes)"
      IO.FS.removeDir d
    else
      let (_, p) ← IO.FS.createTempFile
      IO.println s!"created file ({p.toString.length} bytes)"
      IO.FS.removeFile p
  catch e => IO.println s!"caught: {e}"
  IO.println "after"
