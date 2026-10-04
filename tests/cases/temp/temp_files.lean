/-! V11 row group: `IO.FS.createTempFile` and `IO.FS.createTempDir` (libuv's `uv_os_tmpdir` lookup of
`TMPDIR`, `TMP`, `TEMP`, `TEMPDIR`, then `/tmp`; the `tmp.XXXXXX` name pattern; the file's mode and
read-write handle), `withTempFile` and `withTempDir`, and the lookup's errors. The random part of each
name is never printed. -/

def describe (p : System.FilePath) : String :=
  let s := p.toString
  s!"{s.length - 6} chars then 6: {repr (s.take (s.length - 6)).toString}"

def main (args : List String) : IO Unit := do
  try
    let (h, path) ← IO.FS.createTempFile
    IO.println s!"temp file: {describe path}"
    let md ← path.metadata
    IO.println s!"size {md.byteSize} type {repr md.type}"
    h.putStr "hello temp\nsecond\n"
    h.flush
    h.rewind
    let l ← h.getLine
    IO.println s!"read back: {repr l}"
    IO.println s!"via readFile: {repr (← IO.FS.readFile path)}"
    IO.FS.removeFile path
    IO.println s!"exists after remove: {← path.pathExists}"
    let dir ← IO.FS.createTempDir
    IO.println s!"temp dir: {describe dir}"
    IO.println s!"is dir: {← dir.isDir}"
    IO.FS.writeFile (dir / "inner.txt") "inside"
    IO.println s!"inner: {← IO.FS.readFile (dir / "inner.txt")}"
    IO.FS.removeDirAll dir
    IO.println s!"dir exists after remove: {← dir.pathExists}"
    let r ← IO.FS.withTempFile fun h p => do
      h.putStrLn "scoped"
      h.flush
      return (← IO.FS.readFile p)
    IO.println s!"withTempFile: {repr r}"
    let r ← IO.FS.withTempDir fun d => do
      IO.FS.writeFile (d / "x") "y"
      return (← System.FilePath.readDir d).size
    IO.println s!"withTempDir entries: {r}"
  catch e =>
    IO.println s!"error: {e}"
  if args == ["dir-only"] then
    try
      let d ← IO.FS.createTempDir
      IO.println s!"second dir: {describe d}"
      IO.FS.removeDir d
    catch e =>
      IO.println s!"dir error: {e}"
