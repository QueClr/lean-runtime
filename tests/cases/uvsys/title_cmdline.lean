import Std.Internal.UV.System
/-! `setProcessTitle` natively also writes the title over the process's original `argv` strings
(libuv 1.48's `uv_set_process_title` copies it into `process_title.str`, which is `argv[0]`'s
memory), so `/proc/self/cmdline` shows it. Safe code cannot write that memory: the shared runtime
sets the title `getProcessTitle` returns and the thread's name only, and `/proc/self/cmdline`
keeps the arguments (the alternative outcome). -/
open Std.Internal.UV.System

def main (args : List String) : IO Unit := do
  let before ← IO.FS.readFile "/proc/self/cmdline"
  IO.println s!"cmdline before holds the argument: {decide ((before.splitOn "abcdefghijklmnop").length > 1)}"
  setProcessTitle "new-title"
  IO.println s!"title: {← getProcessTitle}"
  let c ← IO.FS.readFile "/proc/self/cmdline"
  IO.println s!"cmdline starts with the title: {c.startsWith "new-title"}"
  IO.println s!"args: {args}"
