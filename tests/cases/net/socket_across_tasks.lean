import Std.Internal.UV
import Std.Net.Addr
/-! A socket used from other tasks (net-threads). A dedicated task starts a
receive and returns its promise, and then no reference to the socket is
left but the receive's: the bytes the peer sends later still arrive, and
the socket closes right after that receive, so the peer reads the end of
the stream. Then another task cancels a receive that `main` started: the
cancelled receive stays unresolved, and the next receive gets the bytes.
The address comes from argv. -/
open Std.Internal.UV Std.Net

def get {α} [Nonempty α] (p : IO.Promise (Except IO.Error α)) : IO α := do
  match ← IO.wait p.result? with
  | some (.ok v) => pure v
  | some (.error e) => throw e
  | none => throw (IO.userError "dropped")

def text (b : Option ByteArray) : String :=
  match b with
  | none => "the end of the stream"
  | some b => (String.fromUTF8? b).getD "?"

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let addr (port : UInt16) : SocketAddress := .v4 { addr := ip, port }
  let s ← TCP.Socket.new
  s.bind (addr 0)
  s.listen 16
  let port := (← s.getSockName).port
  -- a receive started in a task; the socket's other references go
  let pa ← s.accept
  let c ← TCP.Socket.new
  get (← c.connect (addr port))
  let peer ← get pa
  let t ← IO.asTask (prio := .dedicated) (c.recv? 64)
  let p ← match ← IO.wait t with
    | .ok p => pure p
    | .error e => throw e
  IO.sleep 50
  IO.println s!"the receive is pending: {!(← p.isResolved)}"
  get (← peer.send #["one".toUTF8])
  IO.println s!"the receive got {text (← get p)}"
  IO.println s!"the peer reads {text (← get (← peer.recv? 64))}"
  -- a receive cancelled by another task
  let pa2 ← s.accept
  let d ← TCP.Socket.new
  get (← d.connect (addr port))
  let peer2 ← get pa2
  let p2 ← d.recv? 64
  let k ← IO.asTask (prio := .dedicated) d.cancelRecv
  match ← IO.wait k with
  | .ok () => pure ()
  | .error e => throw e
  IO.println s!"the cancelled receive is resolved: {← p2.isResolved}"
  get (← peer2.send #["two".toUTF8])
  IO.sleep 50
  IO.println s!"after the peer's send: {← p2.isResolved}"
  IO.println s!"the next receive got {text (← get (← d.recv? 64))}"
