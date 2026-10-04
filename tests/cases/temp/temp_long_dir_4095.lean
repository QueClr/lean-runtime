/-! `createTempDir` with a temporary directory of 4095 bytes, the longest libuv accepts: Lean's first
assertion, `lean_always_assert(PATH_MAX >= base_len + 1 + 1)` (io.cpp, 1327), fails: native
aborts, status 134 (LB-16). Both translators give the system's `ENAMETOOLONG` (the expected
outcome). -/
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
