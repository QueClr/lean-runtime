-- A temporary file or directory that cannot be created must be an
-- `IO.Error` (LB-03 in docs/lean-bugs.md). With `TMPDIR` naming a missing
-- directory (<id>.env), `createTempFile` fails with ENOENT, and Lean 4.34
-- decodes it with `decode_uv_error(ret, nullptr)`: native crashes with
-- SIGSEGV (code 139) before the `catch` runs, and its buffered stdout is
-- lost. The expected output is the correct one, `noFileOrDirectory "" 2
-- "no such file or directory"` for both calls, caught; native's is in the
-- case's `native` field.

def describe : IO.Error → String
  | .noFileOrDirectory f c m => s!"noFileOrDirectory {repr f} {c} {repr m}"
  | e => s!"other error: {e}"

def main (args : List String) : IO Unit := do
  IO.println s!"before {args.length}"
  try
    let (_, p) ← IO.FS.createTempFile
    IO.println s!"temp file {p}"
  catch e => IO.println (describe e)
  try
    let p ← IO.FS.createTempDir
    IO.println s!"temp dir {p}"
  catch e => IO.println (describe e)
  IO.println "after"
