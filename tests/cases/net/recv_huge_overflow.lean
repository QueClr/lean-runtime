import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `recv?` and UDP `recv` with a size near 2^64: the buffer is allocated at
the call, before the socket is looked at, with Lean's checked size
arithmetic (`lean_alloc_sarray`): `24 + size` above 2^64 - 1 is
`INTERNAL PANIC: integer overflow in runtime computation`, a size no
allocator grants `INTERNAL PANIC: out of memory`, both with exit 1.
- mode tcp-max: TCP `recv? (2^64 - 1)`;
- mode tcp-25: TCP `recv? (2^64 - 25)` (`24 + size` = 2^64 - 1);
- mode udp-max: UDP `recv (2^64 - 1)`.
The judge's probe (net-1, D_RecvHuge). -/
def main (args : List String) : IO Unit := do
  let mode := args.headD "tcp-max"
  IO.println s!"mode {mode}"
  (← IO.getStdout).flush
  match mode with
  | "tcp-max" =>
    let s ← TCP.Socket.new
    try let _ ← s.recv? (0 - 1); IO.println "returned" catch e => IO.println s!"error: {e}"
  | "tcp-25" =>
    let s ← TCP.Socket.new
    try let _ ← s.recv? (0 - 25); IO.println "returned" catch e => IO.println s!"error: {e}"
  | "udp-max" =>
    let u ← UDP.Socket.new
    try let _ ← u.recv (0 - 1); IO.println "returned" catch e => IO.println s!"error: {e}"
  | _ => IO.println "unknown"
  IO.println "after"
