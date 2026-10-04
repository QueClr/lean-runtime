import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! A second `accept` (mode `accept`) or `tryAccept` (mode `tryAccept`)
while an `accept` is pending fails with `EALREADY`; then a client connects
and the first `accept` gets it. Natively the failing `accept` keeps the
event loop locked (LB-21): the connect never resolves. `tryAccept` unlocks.
Mode `none` makes no second call; mode `thread` has another task create a
socket after the failing `accept`. The judge's probe (net-1, A_Accept). -/
def lo (port : UInt16) : SocketAddress := .v4 { addr := IPv4Addr.ofParts 127 0 0 1, port }

def main (args : List String) : IO Unit := do
  let mode := args.headD "accept"
  let s ← TCP.Socket.new
  s.bind (lo 0)
  s.listen 16
  let port := (← s.getSockName).port
  let p1 ← s.accept
  match mode with
  | "accept" =>
    try let _ ← s.accept; IO.println "second accept: ok"
    catch e => IO.println s!"second accept: {e}"
  | "tryAccept" | "thread" =>
    if mode == "thread" then
      try let _ ← s.accept; IO.println "second accept: ok"
      catch e => IO.println s!"second accept: {e}"
    else
      try let _ ← s.tryAccept; IO.println "tryAccept: ok"
      catch e => IO.println s!"tryAccept: {e}"
  | _ => pure ()
  if mode == "thread" then
    -- Another thread creates a socket: `event_loop_lock` from that thread.
    let t ← IO.asTask (prio := .dedicated) (do let _ ← TCP.Socket.new; pure "other thread: socket created")
    let sl ← IO.asTask (do IO.sleep 2000; pure "other thread: still blocked after 2 s")
    let r ← IO.waitAny [t, sl]
    match r with
    | .ok m => IO.println m
    | .error e => IO.println s!"other thread: {e}"
    (← IO.getStdout).flush
    IO.Process.exit 0
  let c ← TCP.Socket.new
  let pc ← c.connect (lo port)
  IO.println "connect started"
  (← IO.getStdout).flush
  match ← IO.wait pc.result? with
  | some (.ok ()) => IO.println "connect: ok"
  | some (.error e) => IO.println s!"connect: {e}"
  | none => IO.println "connect: dropped"
  match ← IO.wait p1.result? with
  | some (.ok _) => IO.println "first accept: ok"
  | some (.error e) => IO.println s!"first accept: {e}"
  | none => IO.println "first accept: dropped"
