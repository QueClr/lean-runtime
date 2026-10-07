import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `shutdown` while the `connect` is really in progress: the listener's
accept queue is full, so the kernel drops the client's SYN and sends it
again about 1 s later. Natively the `shutdown` feeds libuv's I/O watcher,
which reads `SO_ERROR` = 0 during the handshake and resolves the connect
`ok` at once, before the connection exists (LB-50); the shutdown queued
behind it then never happens (LB-28). Correct: the connect stays pending
until the connection exists; then the shutdown sends FIN and resolves
`ok`, and the server reads the end of the stream. Each wait is bounded.
The networking hunt's repro of HN-01 and HN-02. Only localhost. -/

/-- The state of `p` now: `pending`, or its outcome. -/
def st {α} [Nonempty α] (p : IO.Promise (Except IO.Error α)) : IO String := do
  if ← p.isResolved then
    match ← IO.wait p.result? with
    | some (.ok _) => return "ok"
    | some (.error e) => return s!"error: {e}"
    | none => return "dropped"
  else return "pending"

/-- Waits for `p` and gives its value. -/
def waitOk {α} [Nonempty α] (p : IO.Promise (Except IO.Error α)) : IO α := do
  match ← IO.wait p.result? with
  | some (.ok v) => return v
  | some (.error e) => throw e
  | none => throw (IO.userError "dropped")

def recvRes : Option (Except IO.Error (Option ByteArray)) → String
  | none => "dropped"
  | some (.ok none) => "none (end of stream)"
  | some (.ok (some b)) => s!"some {b.size} bytes"
  | some (.error e) => s!"error: {e}"

/-- Waits up to 2 s for `p`. -/
def waitFor (p : IO.Promise (Except IO.Error (Option ByteArray))) : IO String := do
  for _ in [0:40] do
    if ← p.isResolved then
      return recvRes (← IO.wait p.result?)
    IO.sleep 50
  return "pending"

def main : IO Unit := do
  let lo : IPv4Addr := .ofParts 127 0 0 1
  let srv ← TCP.Socket.new
  srv.bind (.v4 { addr := lo, port := 0 })
  srv.listen 1
  let port := (← srv.getSockName).port
  let addr : SocketAddress := .v4 { addr := lo, port }
  -- The loop accepts c1 as it arrives (libuv accepts with no `accept`
  -- pending, then stops watching); c2 and c3 fill the kernel's accept queue
  -- (a backlog of 1 holds two connections).
  let mut cs : Array TCP.Socket := #[]
  for _ in [0:3] do
    let c ← TCP.Socket.new
    waitOk (← c.connect addr)
    IO.sleep 100
    cs := cs.push c
  -- c4's SYN is dropped: its connect stays in progress.
  let c4 ← TCP.Socket.new
  let pc ← c4.connect addr
  IO.sleep 200
  IO.println s!"connect before shutdown: {← st pc}"
  let ps ← c4.shutdown
  IO.sleep 300
  IO.println s!"connect after shutdown: {← st pc}"
  IO.println s!"shutdown: {← st ps}"
  -- Free the queue: the three connections are accepted and dropped.
  for _ in [0:3] do
    let _ ← waitOk (← srv.accept)
  -- c4's SYN is sent again about 1 s after the first one. Look for the
  -- fourth connection for up to 5 s, and keep it, so that the server side
  -- does not close.
  let mut fourth : Option TCP.Socket := none
  for _ in [0:50] do
    match ← srv.tryAccept with
    | .ok (some s) =>
      fourth := some s
      break
    | _ => IO.sleep 100
  match fourth with
  | none => IO.println "server: no fourth connection"
  | some _ => IO.println "server: a fourth connection arrived"
  try
    let a ← c4.getPeerName
    IO.println s!"c4 getPeerName: ok, port matches {a.port == port}"
  catch e => IO.println s!"c4 getPeerName: {e}"
  IO.println s!"connect at the end: {← st pc}"
  IO.println s!"shutdown at the end: {← st ps}"
  -- The server reads the end of the stream once c4's shutdown has sent FIN.
  match fourth with
  | none => pure ()
  | some s4 => IO.println s!"server recv?: {← waitFor (← s4.recv? 16)}"
  IO.println s!"clients kept: {cs.size}"
