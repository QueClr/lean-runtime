import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `keepAlive` with enable 1 and delay 0: natively `operation not permitted`
(libuv 1.48 returns -1, `UV_EPERM`) where Lean's docstring says `UV_EINVAL`
(LB-23); on a socket without a descriptor it succeeds. Other values as
natively. The judge's probe (net-1, C_KeepAlive). -/
def lo (port : UInt16) : SocketAddress := .v4 { addr := IPv4Addr.ofParts 127 0 0 1, port }

def attempt (what : String) (x : IO Unit) : IO Unit := do
  try x; IO.println s!"{what}: ok" catch e => IO.println s!"{what}: {e}"

def main : IO Unit := do
  let fresh ← TCP.Socket.new
  attempt "fresh (no fd) keepAlive 1 0" (fresh.keepAlive 1 0)
  attempt "fresh bind after keepAlive 1 0" (fresh.bind (lo 0))
  let s ← TCP.Socket.new
  s.bind (lo 0)
  attempt "bound keepAlive 1 0" (s.keepAlive 1 0)
  attempt "bound keepAlive 1 0 again" (s.keepAlive 1 0)
  attempt "bound keepAlive 0 0" (s.keepAlive 0 0)
  attempt "bound keepAlive 1 1" (s.keepAlive 1 1)
  attempt "bound keepAlive 1 40000 (above Linux's TCP_KEEPIDLE max)" (s.keepAlive 1 40000)
  s.listen 4
  let port := (← s.getSockName).port
  let c ← TCP.Socket.new
  let _ ← IO.wait (← c.connect (lo port)).result?
  attempt "connected keepAlive 1 0" (c.keepAlive 1 0)
  attempt "connected keepAlive 1 30" (c.keepAlive 1 30)
  -- `s` is kept until here, so `c` connects to a listening socket (its last
  -- use was the `getSockName` above: it was closed before the connect, and
  -- `c`'s autobind could take its port and connect to itself)
  let _ ← s.getSockName
