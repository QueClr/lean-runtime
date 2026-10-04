-- The modelled errno after a successful `IO.FS.realPath` (glibc's
-- `realpath`), seen through a handle whose error indicator is set: each
-- `getLine` reports the `errno` C holds then. .pipe runs it in a working
-- directory of more than 1024 bytes: a relative path makes glibc call
-- `getcwd` into its 1024-byte scratch buffer first (ERANGE, then it grows),
-- and `readlink` on a component that is not a link leaves EINVAL; an
-- absolute path without components changes nothing (review RIO1-16, the
-- reviewer's RealPathErrno; leanrs review F2). The paths come from argv.

def main (args : List String) : IO Unit := do
  let h ← IO.FS.Handle.mk args[0]! .read
  try h.putStr "x" catch e => IO.println s!"putStr: {e}"
  for p in args.drop 1 do
    let _ ← IO.FS.realPath p
    try
      let l ← h.getLine
      IO.println s!"realPath {p}, getLine ok {repr l}"
    catch e => IO.println s!"realPath {p}, getLine: {e}"
