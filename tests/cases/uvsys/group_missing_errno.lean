import Std.Internal.UV.System
/-! `osGetGroup` of a gid no group has, then a `getLine` that reports the modelled `errno` (a read
handle whose sticky error flag a failed write set). With this host's NSS (`group: files systemd
sss`), `getgrgid_r` reports the missing group as `ENOENT`, which `osGetGroup` turns into `none`
but leaves in `errno`; `getLine` then decodes `ENOENT` without a file name and native crashes
(SIGSEGV, LB-03; leanrs review of io-2). Both translators give LB-03's error with the empty file
name (the expected outcome until LB-41). With `files` alone, `getgrgid_r` leaves 0. Since LB-41
the correct `getLine` reports only its own error, so it reads its line. -/
open Std.Internal.UV.System

def main (args : List String) : IO Unit := do
  let gid := (args.headD "3999999").toNat!.toUInt64
  IO.println "before"
  (← IO.getStdout).flush
  IO.FS.writeFile "e.txt" "line1\n"
  let h ← IO.FS.Handle.mk "e.txt" .read
  try h.putStr "x"; h.flush catch _ => pure ()
  let g ← osGetGroup gid
  IO.println s!"group none {g.isNone}"
  try
    let l ← h.getLine
    IO.println s!"getLine: ok {repr l}"
  catch e => IO.println s!"getLine: {e}"
  IO.println "after"
