import Std.Internal.UV
import Std.Net.Addr
/-! TCP over IPv6 loopback through `Std.Internal.UV.TCP`: bind, listen,
connect, the names of both ends as Lean prints IPv6 addresses, data both
ways, and `shutdown` then end of file. The address comes from argv. -/
open Std.Internal.UV Std.Net

def get {α} [Nonempty α] (p : IO.Promise (Except IO.Error α)) : IO α := do
  match ← IO.wait p.result? with
  | some (.ok v) => return v
  | some (.error e) => throw e
  | none => throw (IO.userError "dropped")

def shown : SocketAddress → String
  | .v4 a => s!"v4 {a.addr}"
  | .v6 a => s!"v6 {a.addr}"

def main (args : List String) : IO Unit := do
  let ip := (IPv6Addr.ofString args[0]!).get!
  let at_ (port : UInt16) : SocketAddress := .v6 { addr := ip, port }
  let s ← TCP.Socket.new
  s.bind (at_ 0)
  s.listen 4
  let port := (← s.getSockName).port
  IO.println s!"server: {shown (← s.getSockName)}, port chosen {port != 0}"
  let c ← TCP.Socket.new
  get (← c.connect (at_ port))
  IO.println s!"client peer: {shown (← c.getPeerName)}, port matches {(← c.getPeerName).port == port}"
  IO.println s!"client name: {shown (← c.getSockName)}"
  let sc ← get (← s.accept)
  IO.println s!"accepted peer: {shown (← sc.getPeerName)}, is the client {(← sc.getPeerName).port == (← c.getSockName).port}"
  get (← c.send #[String.toUTF8 args[1]!])
  let m ← get (← sc.recv? 100)
  IO.println s!"server got {(String.fromUTF8? =<< m).getD "?"}"
  get (← sc.send #[String.toUTF8 "back"])
  get (← sc.shutdown)
  let r ← get (← c.recv? 100)
  IO.println s!"client got {(String.fromUTF8? =<< r).getD "?"}"
  let e ← get (← c.recv? 100)
  IO.println s!"then end of file: {e.isNone}"
  IO.println s!"server still there: {(← s.getSockName).port == port}"
