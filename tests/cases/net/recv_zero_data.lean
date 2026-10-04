import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `recv? 0` (TCP) and `recv 0` (UDP): Lean's buffer of 0 bytes, which libuv
answers with `ENOBUFS` once the socket is readable, without reading.
Modes: data (the peer sent "hello"), eof (the peer shut down, nothing
sent), dataeof (the peer sent "hello", then shut down), udp (a datagram is
pending). At the end of the stream Lean's docstring says `none` (LB-26).
The judge's probe (net-1, R_Recv0). -/
def lo (port : UInt16) : SocketAddress := .v4 { addr := IPv4Addr.ofParts 127 0 0 1, port }

def showRecv : Option (Except IO.Error (Option ByteArray)) → String
  | none => "dropped"
  | some (.ok none) => "ok none"
  | some (.ok (some b)) => s!"ok some {repr (String.fromUTF8? b)}"
  | some (.error e) => s!"error {e}"

def pair : IO (TCP.Socket × TCP.Socket × TCP.Socket) := do
  let s ← TCP.Socket.new
  s.bind (lo 0)
  s.listen 16
  let port := (← s.getSockName).port
  let c ← TCP.Socket.new
  let pc ← c.connect (lo port)
  let pa ← s.accept
  let _ ← IO.wait pc.result?
  match ← IO.wait pa.result? with
  | some (.ok peer) => return (s, c, peer)
  | _ => throw <| IO.userError "accept failed"

def recv (c : TCP.Socket) (n : UInt64) : IO Unit :=
  do IO.println s!"recv? {n}: {showRecv (← IO.wait (← c.recv? n).result?)}"

def main (args : List String) : IO Unit := do
  match args.headD "data" with
  | "data" =>
    let (_s, c, peer) ← pair
    let _ ← IO.wait (← peer.send #["hello".toUTF8]).result?
    recv c 0; recv c 0; recv c 16
  | "eof" =>
    let (_s, c, peer) ← pair
    let _ ← IO.wait (← peer.shutdown).result?
    recv c 0; recv c 16; recv c 0
  | "dataeof" =>
    let (_s, c, peer) ← pair
    let _ ← IO.wait (← peer.send #["hello".toUTF8]).result?
    let _ ← IO.wait (← peer.shutdown).result?
    recv c 0; recv c 16; recv c 0; recv c 16
  | "udp" =>
    let u ← UDP.Socket.new
    u.bind (lo 0)
    let v ← UDP.Socket.new
    v.bind (lo 0)
    let _ ← IO.wait (← v.send #["hi".toUTF8] (some (← u.getSockName))).result?
    for n in [0, 16] do
      match ← IO.wait (← u.recv n.toUInt64).result? with
      | some (.ok (b, a)) => IO.println s!"recv {n}: ok {repr (String.fromUTF8? b)} from sender {a.isSome}"
      | some (.error e) => IO.println s!"recv {n}: error {e}"
      | none => IO.println s!"recv {n}: dropped"
  | m => IO.println s!"unknown {m}"
