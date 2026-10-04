/-! `createTempFile` with a temporary directory of 4090 bytes: Lean's `lean_always_assert(PATH_MAX >=
strlen(path) + file_pattern_size + 1)` (io.cpp, 1288) fails: native aborts with `LEAN ASSERTION
VIOLATION`, status 134 (LB-16). Both translators give the system's `ENAMETOOLONG` (the
alternative outcome). -/
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
