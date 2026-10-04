import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `shutdown` requested while `connect` is still pending. Natively, with no
write queued, the shutdown never happens (LB-28): the connect resolves,
the shutdown's promise never does, and the peer never reads end of stream.
The judge's probe (net-1 review RNET-02). Modes:
  early      shutdown right after connect (nothing written)
  write      connect, send "x", shutdown, all before the connect resolves (control)
  after      connect, wait for it, then shutdown (control)
Each wait is bounded: a promise still pending after 2 s is reported as `pending`. -/

def lo (port : UInt16) : SocketAddress := .v4 { addr := IPv4Addr.ofParts 127 0 0 1, port }

def say (s : String) : IO Unit := do IO.println s; (← IO.getStdout).flush

/-- Waits up to 2 s for `p`. -/
def waitFor {α : Type} (p : IO.Promise α) (fmt : α → String) : IO String := do
  for _ in [0:40] do
    if ← IO.hasFinished p.result? then
      return match ← IO.wait p.result? with
        | some v => fmt v
        | none => "dropped"
    IO.sleep 50
  return "pending"

def unitRes : Except IO.Error Unit → String
  | .ok () => "ok"
  | .error e => s!"error {e}"

def recvRes : Except IO.Error (Option ByteArray) → String
  | .ok none => "ok none (end of stream)"
  | .ok (some b) => s!"ok some {repr (String.fromUTF8? b)}"
  | .error e => s!"error {e}"

def main (args : List String) : IO Unit := do
  let mode := args.headD "early"
  let s ← TCP.Socket.new
  s.bind (lo 0)
  s.listen 16
  let port := (← s.getSockName).port
  let pa ← s.accept
  let c ← TCP.Socket.new
  let pc ← c.connect (lo port)
  let ps ← match mode with
    | "after" => do
      say s!"connect: {← waitFor pc unitRes}"
      c.shutdown
    | "write" => do
      let _ ← c.send #["x".toUTF8]
      c.shutdown
    | _ => c.shutdown
  if mode != "after" then
    say s!"connect: {← waitFor pc unitRes}"
  let peer ← match ← IO.wait pa.result? with
    | some (.ok p) => pure p
    | _ => throw (IO.userError "accept failed")
  -- The program's last use of `c` is above: it holds no reference from here on.
  say s!"shutdown: {← waitFor ps unitRes}"
  say s!"peer recv?: {← waitFor (← peer.recv? 64) recvRes}"
  if mode == "write" then
    say s!"peer recv?: {← waitFor (← peer.recv? 64) recvRes}"
  say "done"
