import Std.Internal.UV.System
/-! libuv's own checks and the fixed buffers Lean passes in
`Std.Internal.UV.System` (lean2rr's `RtUvSysLimits` without its DNS part, which networking covers). `osHomedir` is `HOME` even
when it is empty, `osTmpdir` the first of `TMPDIR`, `TMP`, `TEMP`,
`TEMPDIR` that is set, even empty; a value of `PATH_MAX` (4096) bytes or
more is `ENOBUFS`, and so is a process title of 512 bytes or more;
priorities outside [-20, 19] (as a whole `Int64`; natively after a cut to an
`int`, LB-45, which rejects these values too) and `random` of more
than 0x7FFFFFFF bytes are rejected at once. `RtUvSysLimits.pipe` runs the program (mode `env`)
under several environments, then with a long argument (room for a long
title). -/
open Std.Internal.UV.System

def tryIO (label : String) (x : IO String) : IO Unit := do
  try IO.println s!"{label}: {← x}" catch e => IO.println s!"{label}: error: {e}"

def brief (s : String) : String := s!"{s.utf8ByteSize} {(s.take 12).copy.quote}"

def title (n : Nat) : IO Unit :=
  tryIO s!"title of {n}" do
    setProcessTitle (String.ofList (List.replicate n 'T'))
    return toString (← getProcessTitle).utf8ByteSize

def main (args : List String) : IO Unit := do
  tryIO "osHomedir" do return brief (← osHomedir)
  tryIO "osTmpdir" do return brief (← osTmpdir)
  if args == ["env"] then return
  let p ← osGetPriority 0
  for q in ([20, -21, 100, -100, 4294967296 + 25, 4294967296 * 5 - 30] : List Int64) do
    tryIO s!"setPriority {q}" do osSetPriority 0 q; return "ok"
  IO.println s!"priority unchanged {(← osGetPriority 0) == p}"
  title 600
  title 511
  title 512
  tryIO "random 0x80000000" do
    let r ← random 0x80000000
    return s!"started {← IO.hasFinished r.result?}"
