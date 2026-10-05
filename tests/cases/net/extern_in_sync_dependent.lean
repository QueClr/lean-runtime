import Std.Internal.UV
import Std.Net.Addr
/-! A `sync` dependent of a network promise calls externs, and UDP sockets
and name lookups are used from several tasks (net-threads). The dependent
of a receive's promise runs where the promise resolves (natively on the
event loop's thread, which holds the loop's lock): it sets `noDelay` and
sends a reply on the same socket, which the peer then reads. Then four
dedicated tasks each send a datagram to one UDP socket, which `main` reads
four times (the texts printed sorted: their order is the schedule's), and
three dedicated tasks look up `localhost` at once. The address comes from
argv. -/
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
  -- a sync dependent of a receive's promise calls externs on its socket
  let s ← TCP.Socket.new
  s.bind (addr 0)
  s.listen 16
  let port := (← s.getSockName).port
  let pa ← s.accept
  let c ← TCP.Socket.new
  get (← c.connect (addr port))
  let peer ← get pa
  let p ← c.recv? 64
  let dep ← IO.mapTask (sync := true) (fun r => do
      c.noDelay
      let _ ← c.send #["pong".toUTF8]
      return match r with
        | some (.ok b) => text b
        | _ => "?") p.result?
  get (← peer.send #["ping".toUTF8])
  match ← IO.wait dep with
  | .ok m => IO.println s!"the dependent got {m} and replied"
  | .error e => IO.println s!"the dependent: {e}"
  IO.println s!"the peer reads {text (← get (← peer.recv? 64))}"
  -- datagrams from several tasks
  let u ← UDP.Socket.new
  u.bind (addr 0)
  let uport := (← u.getSockName).port
  let senders ← ["d1", "d2", "d3", "d4"].mapM fun n =>
    IO.asTask (prio := .dedicated) do
      let v ← UDP.Socket.new
      v.bind (addr 0)
      get (← v.send #[n.toUTF8] (some (addr uport)))
  for t in senders do
    match ← IO.wait t with
    | .ok () => pure ()
    | .error e => IO.println s!"sender: {e}"
  let mut got : Array String := #[]
  for _ in [0:4] do
    let (b, _) ← get (← u.recv 64)
    got := got.push ((String.fromUTF8? b).getD "?")
  IO.println s!"the datagrams: {got.qsort (· < ·)}"
  -- lookups from several tasks
  let lookups ← [0, 1, 2].mapM fun (_ : Nat) =>
    IO.asTask (prio := .dedicated) do
      let a ← get (← DNS.getAddrInfo "localhost" "" 1)
      return a.map toString
  for t in lookups do
    match ← IO.wait t with
    | .ok a => IO.println s!"lookup: {a}"
    | .error e => IO.println s!"lookup: {e}"
