import Std.Async
import Std.Net.Addr
/-! TCP on the loopback through `Std.Async.TCP` (over `Std.Internal.UV.TCP`;
lean2rr's `tests/runtime/RtTcp.lean`): a server task and clients in the
`Async` style and in a blocking style (a task that waits for `accept` or
`recv?` lets the others run); end of file after `shutdown`; a 16 MiB send
queued in pieces, a `shutdown` behind it and a `send` after the `shutdown`;
`tryAccept` with nobody connecting. The address and the client names come
from argv; ports are chosen by the system (port 0) and read back with
`getSockName`. Every socket is used at the end, so none is closed early. -/
open Std.Async
open Std.Net

def str (b : Option ByteArray) : String := ((String.fromUTF8? =<< b).getD "<none>")

def echoServer (s : TCP.Socket.Server) (n : Nat) : Async Unit := do
  for _ in [0:n] do
    let c ← s.accept
    let m ← c.recv? 1024
    c.send (String.toUTF8 s!"echo {str m}")
    c.shutdown

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let at_ (port : UInt16) : SocketAddress := .v4 { addr := ip, port }
  let s ← TCP.Socket.Server.mk
  s.bind (at_ 0)
  s.listen 16
  let port := (← s.getSockName).port
  IO.println s!"listening on a port: {decide (port > 0)}"
  -- Async style: a server task and two clients
  let st ← (echoServer s 2).toIO
  for name in args.drop 1 do
    let c ← TCP.Socket.Client.mk
    (← c.connect (at_ port) |>.toBaseIO).block
    IO.println s!"peer port matches: {(← c.getPeerName).port == port}"
    c.noDelay
    (c.send (String.toUTF8 name)).block
    IO.println s!"client got {str (← (c.recv? 1024).block)}"
    IO.println s!"then {str (← (c.recv? 1024).block)}"
  st.block
  -- blocking style: the server is a task that blocks on accept and recv
  let srv ← IO.asTask (prio := .dedicated) do
    let c ← s.accept.block
    let m ← (c.recv? 1024).block
    IO.println s!"server got {str m}"
    (c.send (String.toUTF8 "pong")).block
    let m2 ← (c.recv? 1024).block
    IO.println s!"server got {str m2} after the client's shutdown"
  let c ← TCP.Socket.Client.mk
  (← c.connect (at_ port) |>.toBaseIO).block
  (c.send (String.toUTF8 "ping")).block
  IO.println s!"client got {str (← (c.recv? 1024).block)}"
  c.shutdown.block
  IO.ofExcept (← IO.wait srv)
  -- a send after a shutdown while a large write is still queued fails:
  -- `uv_shutdown` stops the writes at once
  let srv2 ← IO.asTask (prio := .dedicated) do
    let c ← s.accept.block
    IO.sleep 50
    let mut total := 0
    repeat
      match ← (c.recv? 65536).block with
      | none => break
      | some b => total := total + b.size
    return total
  let c4 ← TCP.Socket.Client.mk
  (← c4.connect (at_ port) |>.toBaseIO).block
  let raw4 := c4.native
  let big := ByteArray.mk (Array.replicate (16 * 1024 * 1024) 7)
  let p1 ← raw4.send #[big]
  let p2 ← raw4.shutdown
  try
    let p3 ← raw4.send #[String.toUTF8 "x"]
    IO.println s!"send after shutdown accepted: {(← IO.wait p3.result!).isOk}"
  catch e => IO.println s!"send after shutdown: {e}"
  IO.println s!"big send ok: {(← IO.wait p1.result!).isOk}, shutdown ok: {(← IO.wait p2.result!).isOk}"
  match ← IO.wait srv2 with
  | .ok n => IO.println s!"server received {n}"
  | .error e => IO.println s!"server: {e}"
  -- tryAccept with nobody connecting
  match ← s.tryAccept with
  | none => IO.println "tryAccept: none"
  | some _ => IO.println "tryAccept: a socket?"
  IO.println s!"server still listening on the port: {(← s.getSockName).port == port}"
