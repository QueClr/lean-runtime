import Std.Internal.UV.System
/-! `getProcessTitle` and `setProcessTitle` (libuv 1.48's `uv_setup_args`,
`uv_get_process_title`, `uv_set_process_title`; Lean's `lean_setup_args` calls
`uv_setup_args` before `main`). The title starts as `argv[0]`; a new one is cut to the memory of
the original arguments (their bytes and NULs, less one) and becomes the calling thread's name
(`prctl(PR_SET_NAME)`, first 15 bytes). A title holding a NUL byte is Lean's embedded-NUL error.
The `.pipe` runs it with a long argument (room for every title), then as `argv = P title` (room
for 7 bytes). Native libuv also writes the title over the original `argv` memory, so
`/proc/self/cmdline` changes; that is not observed here. -/
open Std.Internal.UV.System

def quoted (s : String) : String := (s.replace "\x00" "|").quote

def main (args : List String) : IO Unit := do
  let t ← getProcessTitle
  IO.println s!"initial: {if args == ["title"] then t else toString (t.endsWith "process_title")}"
  setProcessTitle "short"
  IO.println s!"after set: {quoted (← getProcessTitle)} thread name {quoted (← IO.FS.readFile "/proc/thread-self/comm")}"
  setProcessTitle "a-title-longer-than-sixteen-bytes-and-more-and-more-and-more"
  IO.println s!"after long: {quoted (← getProcessTitle)} thread name {quoted (← IO.FS.readFile "/proc/thread-self/comm")}"
  try setProcessTitle "nul\x00inside" catch e => IO.println s!"nul: {e}"
  IO.println s!"after nul: {quoted (← getProcessTitle)}"
  setProcessTitle ""
  IO.println s!"empty: {quoted (← getProcessTitle)} thread name {quoted (← IO.FS.readFile "/proc/thread-self/comm")}"
