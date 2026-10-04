/-! `createTempFile` and `createTempDir` in a directory that does not exist: `mkostemp` and
`mkdtemp` fail with `ENOENT`, which io.cpp decodes without a file name
(`decode_uv_error(ret, nullptr)`), and the decoder dereferences the null name: native crashes
(SIGSEGV, status 139, stdout lost). LB-03: both translators give the error with the empty file
name instead (the alternative outcome). -/
def main : IO Unit := do
  IO.println "before"
  try
    let (_, p) ← IO.FS.createTempFile
    IO.println s!"created {p}"
  catch e => IO.println s!"file: {e}"
  try
    let d ← IO.FS.createTempDir
    IO.println s!"created {d}"
  catch e => IO.println s!"dir: {e}"
  IO.println "after"
