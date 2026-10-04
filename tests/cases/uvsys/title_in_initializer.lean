import Std.Internal.UV.System
/-! A module initializer sets a title longer than the arguments' memory, so libuv 1.48 writes 'é's
over every argument and cuts the last one, maybe in the middle of a character. `main`'s `args` still
hold the original arguments: the generated `main` builds them from libuv's copy of the arguments
(`uv_setup_args`'s `args_mem`), after the initializers. -/
open Std.Internal.UV.System

initialize setProcessTitle ("".pushn 'é' 300)

def main (args : List String) : IO Unit := do
  IO.println s!"args: {args}"
  let c ← IO.FS.readBinFile "/proc/self/cmdline"
  IO.println s!"cmdline starts with the title: {c.data.toList.take 4 == [0xC3, 0xA9, 0xC3, 0xA9]}"
