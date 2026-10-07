import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `TCP.Socket.waitReadable` at the end of the stream. Its docstring says
the promise is `true` once the socket has data to read, and `false` if the
socket is closed before that; Lean's read callback has a branch for
libuv's `UV_EOF` that gives `false`. But libuv answers Lean's empty buffer
with `UV_ENOBUFS` before it reads, so natively the promise is `true` at the
end of the stream too (LB-51). Correct: `true` while bytes are unread (also
with the end of the stream behind them), `false` at the end of the stream
with nothing left. Moved from case `tcp_errors` (its line "waitReadable at
end of file"); `recv? 0` is LB-26's case `recv_zero_eof`. -/

def lo (port : UInt16) : SocketAddress := .v4 { addr := IPv4Addr.ofParts 127 0 0 1, port }

def showWait : Option (Except IO.Error Bool) → String
  | none => "dropped"
  | some (.ok b) => s!"ok {b}"
  | some (.error e) => s!"error {e}"

def showRecv : Option (Except IO.Error (Option ByteArray)) → String
  | none => "dropped"
  | some (.ok none) => "ok none"
  | some (.ok (some b)) => s!"ok some {repr (String.fromUTF8? b)}"
  | some (.error e) => s!"error {e}"

/-- A listening socket, a client and the accepted peer. -/
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

def waitR (c : TCP.Socket) : IO Unit :=
  do IO.println s!"waitReadable: {showWait (← IO.wait (← c.waitReadable).result?)}"

def recv (c : TCP.Socket) : IO Unit :=
  do IO.println s!"recv? 16: {showRecv (← IO.wait (← c.recv? 16).result?)}"

def main : IO Unit := do
  -- the peer shuts down its side without sending anything
  IO.println "the peer shut down, nothing sent"
  let (_s, c, peer) ← pair
  let _ ← IO.wait (← peer.shutdown).result?
  waitR c
  recv c
  -- the peer sends "hello", then shuts down its side
  IO.println "the peer sent hello, then shut down"
  let (_s2, d, peer2) ← pair
  let _ ← IO.wait (← peer2.send #["hello".toUTF8]).result?
  let _ ← IO.wait (← peer2.shutdown).result?
  waitR d
  recv d
  waitR d
  recv d
