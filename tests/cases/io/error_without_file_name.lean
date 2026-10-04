-- An error without a file name must be an `IO.Error` (LB-03 in
-- docs/lean-bugs.md). `getCurrentDir` after the working directory was
-- removed fails with ENOENT, and Lean 4.34's `decode_io_error` builds the
-- `noFileOrDirectory` error from a null file name: native crashes with
-- SIGSEGV (code 139) before the `catch` runs, and its buffered stdout is
-- lost. The expected output is the correct one, the error
-- `noFileOrDirectory "" 2 "no such file or directory"`, caught; native's is
-- in the case's `native` field.

def main (args : List String) : IO Unit := do
  let d : System.FilePath := s!"gone-{args.length}"
  IO.FS.createDirAll d
  let abs ← IO.FS.realPath d
  IO.println "before"
  IO.Process.setCurrentDir abs
  IO.FS.removeDir abs
  try
    let c ← IO.Process.getCurrentDir
    IO.println s!"cwd {c}"
  catch e =>
    match e with
    | .noFileOrDirectory f c m => IO.println s!"noFileOrDirectory {repr f} {c} {repr m}"
    | e => IO.println s!"other error: {e}"
  IO.println "after"
