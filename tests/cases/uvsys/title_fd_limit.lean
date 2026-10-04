import Std.Internal.UV.System
/-! `setProcessTitle` when libuv's startup descriptors leave one descriptor free. The generated
`main` hands the arguments to `lean_setup_args` before libuv opens its startup descriptors, so the
title is written into the arguments' memory however few descriptors are left after them. .pipe runs
the program under `ulimit -n 12`: descriptors 0 to 10 are open at startup (the standard three and
libuv's eight), and the listing of `/proc/self/fd` takes the last one, 11. The title comes from
argv. -/
open Std.Internal.UV.System

def main (args : List String) : IO Unit := do
  let entries ← System.FilePath.readDir "/proc/self/fd"
  let mut fds : Array Nat := #[]
  for e in entries do
    if let some n := e.fileName.toNat? then fds := fds.push n
  IO.println s!"open at startup: {fds.qsort (· < ·)}"
  let title := args.headD "none"
  setProcessTitle title
  IO.println s!"title: {← getProcessTitle}"
  let c ← IO.FS.readFile "/proc/self/cmdline"
  IO.println s!"cmdline starts with the title: {c.startsWith title}"
  IO.println s!"args: {args}"
