import Std.Internal.UV
import Std.Net.Addr
/-! 64 UDP sockets, each bound, with a `recv` cancelled by `cancelRecv`,
then dropped: their descriptors must be closed. Natively `cancelRecv` keeps
the reference `recv` took for the event loop, so every socket stays open
(LB-24). Upstream's test shape: `ok` when at most 16 more descriptors are
open, else the count. The address comes from argv. -/
open Std.Internal.UV Std.Net

def openFds : IO Nat := return (← System.FilePath.readDir "/dev/fd").size

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let before ← openFds
  for _ in [0:64] do
    let u ← UDP.Socket.new
    u.bind (.v4 { addr := ip, port := 0 })
    let _ ← u.recv 64
    u.cancelRecv
  let n := (← openFds) - before
  IO.println (if n ≤ 16 then "ok" else s!"leaked {n} file descriptors")
