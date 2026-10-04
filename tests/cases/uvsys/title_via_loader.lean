import Std.Internal.UV.System
/-! The program started through the dynamic loader (`ld.so ./prog`; `.pipe`): natively the generated
`main` still hands its arguments to `lean_setup_args`, so `setProcessTitle` writes the title over
them, and `/proc/self/cmdline` (the loader's path, then the program's arguments) holds it. The shared
runtime cannot tell the program's executable from the loader in that launch (the kernel's record of
the code, `start_code` to `end_code`, is then the loader's), so it keeps no arguments' memory and the
title fails with `ENOBUFS` (`title_via_loader.alt1.*`; deviation LQ1-01). -/
open Std.Internal.UV.System

def main (args : List String) : IO Unit := do
  try
    setProcessTitle "loader-title"
    IO.println "title set"
  catch e => IO.println s!"title not set: {e}"
  let c ← IO.FS.readFile "/proc/self/cmdline"
  IO.println s!"cmdline holds the title: {decide ((c.splitOn "loader-title").length > 1)}"
  IO.println s!"args: {args}"
