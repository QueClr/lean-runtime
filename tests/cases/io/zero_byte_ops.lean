-- Writing zero bytes to stdin is a no-op natively (`fwrite` returns at once),
-- so it sets no error indicator and a later `getLine` works; reading zero
-- bytes from stdout is a no-op too (`lean_io_prim_handle_read` returns before
-- `fread`). The empty texts come from argv (from lean2rr's
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
