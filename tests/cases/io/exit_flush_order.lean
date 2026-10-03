-- At exit (here `IO.Process.exit`), compiled Lean flushes standard output
-- first (libc++'s `ios_base::Init` atexit handler), then the open file
-- handles, newest first (glibc `_IO_cleanup`). A handle opened on
-- /dev/stdout therefore prints after main's own buffered output, although
-- it was written later only by a hair (finding A811, leanrs; repro
-- ReExitOrder).

def main (args : List String) : IO Unit := do
  let h ← IO.FS.Handle.mk args.head! .append
  IO.print "A\n"
  h.putStr "B\n"
  IO.Process.exit 0
  h.putStr "never\n"
