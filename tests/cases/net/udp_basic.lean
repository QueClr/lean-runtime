import Std.Async
import Std.Net.Addr
/-! UDP on the loopback through `Std.Async.UDP` (over
`Std.Internal.UV.UDP`; lean2rr's `tests/runtime/RtUdp.lean`): datagrams with
the sender's address, a receiver waiting in a task while `main` sends, a
connected socket, `waitReadable`, truncation to the buffer's size, options,
and errors as native Lean reports them: sending without an address on an
unconnected socket, with one on a connected socket, connecting twice, an
invalid TTL, options and names on a socket with no descriptor yet, the peer
of an unconnected socket. The address comes from argv; ports are chosen by
the system. -/
open Std.Async
open Std.Net

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let at_ (port : UInt16) : SocketAddress := .v4 { addr := ip, port }
  let a ← UDP.Socket.mk
  a.bind (at_ 0)
  let pa := (← a.getSockName).port
  let b ← UDP.Socket.mk
  b.bind (at_ 0)
  let pb := (← b.getSockName).port
  (a.send (String.toUTF8 "ping") (some (at_ pb))).block
  let (m, frm) ← (b.recv 100).block
  IO.println s!"b got {String.fromUTF8? m} from a: {frm.map (·.port) == some pa}"
  -- a receiver waiting in a task while main sends
  let t ← IO.asTask (prio := .dedicated) do
    let (m, _) ← (a.recv 100).block
    return String.fromUTF8? m
  IO.sleep 20
  (b.send (String.toUTF8 "pong") (some (at_ pa))).block
  IO.println s!"a got {← IO.ofExcept (← IO.wait t)}"
  -- a connected socket
  let c ← UDP.Socket.mk
  c.bind (at_ 0)
  c.connect (at_ pb)
  IO.println s!"peer: {(← c.getPeerName).port == pb}"
  (c.send (String.toUTF8 "connected")).block
  let (m, _) ← (b.recv 100).block
  IO.println s!"b got {String.fromUTF8? m}"
  -- truncation to the buffer size, waitReadable
  (a.send (String.toUTF8 "a long datagram") (some (at_ pb))).block
  let w ← b.native.waitReadable
  IO.println s!"readable: {(← IO.wait w.result!) matches .ok ()}"
  let (m, _) ← (b.recv 6).block
  IO.println s!"truncated: {String.fromUTF8? m}"
  -- options
  b.setBroadcast true
  b.setTTL 64
  b.setMulticastTTL 2
  b.setMulticastLoop false
  IO.println "options set"
  -- errors
  let d ← UDP.Socket.mk
  try (d.send (String.toUTF8 "x")).block catch e => IO.println s!"send without address: {e}"
  try (c.send (String.toUTF8 "x") (some (at_ pa))).block catch e => IO.println s!"send with address when connected: {e}"
  try c.connect (at_ pa) catch e => IO.println s!"connect twice: {e}"
  try b.setTTL 0 catch e => IO.println s!"TTL 0: {e}"
  try d.setBroadcast true catch e => IO.println s!"option without a descriptor: {e}"
  try discard <| d.getSockName catch e => IO.println s!"name without a descriptor: {e}"
  try discard <| b.getPeerName catch e => IO.println s!"peer of an unconnected socket: {e}"
  IO.println s!"sockets kept: {(← a.getSockName).port == pa} {(← b.getSockName).port == pb}"
