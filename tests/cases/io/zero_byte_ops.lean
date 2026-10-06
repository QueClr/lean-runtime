-- Writing zero bytes to stdin is a no-op natively (`fwrite` returns at once),
-- so `putStr` and `write` succeed (a byte would fail with EBADF: stdin is
-- read-only) and a later `getLine` reads its line; reading zero bytes from
-- stdout is a no-op too (`lean_io_prim_handle_read` returns before `fread`).
-- Before LB-41 the `getLine` also showed that no error indicator was set;
-- since LB-41 `getLine` reports only its own error, so the unit test
-- `an_empty_put_sets_no_error_indicator` (src/io/cfile_tests.rs) checks the
-- indicator. The empty texts come from argv (from lean2rr's
-- tests/runtime/RtStdioStdinEmptyPut.lean).

def main (args : List String) : IO Unit := do
  let stdin ← IO.getStdin
  stdin.putStr args.head!
  stdin.write args.head!.toUTF8
  let l ← stdin.getLine
  IO.println s!"got {repr l}"
  let out ← IO.getStdout
  let b ← out.read (args.length - 1).toUSize
  IO.println s!"read 0 from stdout: {b.size}"
