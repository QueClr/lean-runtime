import Std.Internal.UV
import Std.Net.Addr
/-! 64 TCP sockets, each bound but not connected, with a `shutdown` that
fails (`ENOTCONN`), then dropped: their descriptors must be closed.
Natively the failing `shutdown` keeps the reference it took for the event
loop, so every socket stays open (LB-25). A first TCP socket is made before
counting (the first one also opens libuv's spare descriptor). Upstream's
test shape: `ok` when at most 16 more descriptors are open, else the count.
The address comes from argv. -/
open Std.Internal.UV Std.Net

def openFds : IO Nat := return (← System.FilePath.readDir "/dev/fd").size

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let first ← TCP.Socket.new
  let before ← openFds
  for _ in [0:64] do
    let t ← TCP.Socket.new
    t.bind (.v4 { addr := ip, port := 0 })
    try let _ ← t.shutdown catch _ => pure ()
  let n := (← openFds) - before
  IO.println (if n ≤ 16 then "ok" else s!"leaked {n} file descriptors")
  -- `first` is kept until here
  try let _ ← first.getSockName catch _ => pure ()
