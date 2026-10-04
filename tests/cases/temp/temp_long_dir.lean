/-! `createTempDir` with a temporary directory of 4083 to 4095 bytes: libuv accepts it (it is below
`PATH_MAX`), but Lean's `lean_always_assert(PATH_MAX >= strlen(path) + file_pattern_size + 1)`
(io.cpp, 1334) fails once `/tmp.XXXXXXXX` is appended: native aborts with `LEAN ASSERTION
VIOLATION`, status 134 (LB-16). Both translators let the system answer for the path:
`ENAMETOOLONG`, the alternative outcome. -/
def main : IO Unit := do
  IO.println "before"
  (← IO.getStdout).flush
  try
    let d ← IO.FS.createTempDir
    IO.println s!"created {d}"
  catch e => IO.println s!"dir: {e}"
  IO.println "after"
