import Std.Internal.UV
import Std.Net.Addr
/-! `Std.Internal.UV.TCP.Socket` edge cases, as libuv 1.48 and Linux give
them through Lean 4.34.0's externs: errors before a descriptor exists (a new
socket has none until `bind`, `connect` or `listen`); keep-alive values; a
bind to a port in use reported later (libuv's delayed error); one receive at
a time (`EALREADY`); `connect` again once connected; `waitReadable` and
`cancelRecv`; `shutdown` twice (pending behind a large write the peer has
not read, then done) and `send` after it; end of file twice; a refused
connect, then `ECONNABORTED`; IPv4 to IPv6; buffers with empty ones in one
`send`; an `accept` of a connection the loop already took. A delay of 0 is
case `keepalive_zero_delay`, a size of 0 case `recv_zero`, `waitReadable` at
end of file case `wait_readable_eof`. The address and the message come from
argv. -/
open Std.Internal.UV Std.Net

def tryIO (name : String) (act : IO String) : IO Unit := do
  try
    let s ← act
    IO.println s!"{name}: ok{s}"
  catch e => IO.println s!"{name}: {e}"

def wait {α} [Nonempty α] (p : IO.Promise (Except IO.Error α)) (f : α → String) : IO String := do
  match ← IO.wait p.result? with
  | none => return " (dropped)"
  | some (.ok v) => return s!", then {f v}"
  | some (.error e) => return s!", then {e}"

def unit (_ : Unit) : String := "ok"
def bytes (b : Option ByteArray) : String :=
  match b with
  | none => "none"
  | some b => s!"some {(String.fromUTF8? b).getD "?"}"

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let at_ (port : UInt16) : SocketAddress := .v4 { addr := ip, port }
  -- a new socket
  let u ← TCP.Socket.new
  tryIO "getPeerName new" do let _ ← u.getPeerName; return ""
  tryIO "getSockName new" do let _ ← u.getSockName; return ""
  tryIO "send new" do let _ ← u.send #[.mk #[1]]; return ""
  tryIO "send new, no buffers" do let p ← u.send #[]; wait p unit
  tryIO "recv? new" do let _ ← u.recv? 10; return ""
  tryIO "waitReadable new" do let _ ← u.waitReadable; return ""
  tryIO "shutdown new" do let _ ← u.shutdown; return ""
  tryIO "tryAccept new" do
    match ← u.tryAccept with
    | .ok none => return ", none"
    | .ok (some _) => return ", some"
    | .error e => return s!", {e}"
  tryIO "noDelay new" do u.noDelay; return ""
  tryIO "keepAlive 1 5 new" do u.keepAlive 1 5; return ""
  tryIO "cancelRecv new" do u.cancelRecv; return ""
  tryIO "cancelAccept new" do u.cancelAccept; return ""
  -- a bound socket
  let s ← TCP.Socket.new
  s.bind (at_ 0)
  tryIO "keepAlive 1 40000" do s.keepAlive 1 40000; return ""
  tryIO "keepAlive 0 0" do s.keepAlive 0 0; return ""
  tryIO "keepAlive 1 5" do s.keepAlive 1 5; return ""
  tryIO "noDelay" do s.noDelay; return ""
  tryIO "getPeerName bound" do let _ ← s.getPeerName; return ""
  tryIO "bind again" do s.bind (at_ 0); return ""
  s.listen 16
  let port := (← s.getSockName).port
  tryIO "recv? listening" do let _ ← s.recv? 10; return ""
  tryIO "shutdown listening" do let _ ← s.shutdown; return ""
  tryIO "send listening" do let _ ← s.send #[.mk #[1]]; return ""
  tryIO "connect listening" do let p ← s.connect (at_ port); wait p unit
  -- a client
  let c ← TCP.Socket.new
  let pc ← c.connect (at_ port)
  IO.println s!"connect{← wait pc unit}"
  tryIO "connect again" do let p ← c.connect (at_ port); wait p unit
  tryIO "connect a third time" do let p ← c.connect (at_ port); wait p unit
  IO.sleep 50
  let pa ← s.accept
  IO.println s!"accept of a connection the loop took: resolved at once {← pa.isResolved}"
  let sc ← match ← IO.wait pa.result? with
    | some (.ok v) => pure v
    | _ => throw (IO.userError "accept failed")
  IO.println s!"peer of the accepted socket is the client: {(← sc.getPeerName).port == (← c.getSockName).port}"
  -- one receive at a time (nothing to read yet, so the first stays pending)
  tryIO "waitReadable while a recv? is pending" do
    let _ ← c.recv? 100
    try
      let _ ← c.waitReadable
      return ", waitReadable too?"
    catch e =>
      c.cancelRecv
      return s!", {e}"
  -- waitReadable, cancelled: its promise stays pending while held
  let w ← c.waitReadable
  c.cancelRecv
  let _ ← sc.send #[String.toUTF8 args[1]!]
  IO.sleep 50
  IO.println s!"cancelled waitReadable resolved: {← w.isResolved}"
  tryIO "waitReadable" do let p ← c.waitReadable; wait p toString
  tryIO "recv? 3" do let p ← c.recv? 3; wait p bytes
  tryIO "recv? 100" do let p ← c.recv? 100; wait p bytes
  -- several buffers, empty ones among them, in one send
  tryIO "send of 4 buffers" do
    let p ← c.send #[String.toUTF8 "a", .empty, String.toUTF8 "bc", .empty]
    let r ← wait p unit
    let q ← sc.recv? 100
    return s!"{r}{← wait q bytes}"
  -- shutdown behind a write the peer does not read yet: it stays pending
  let big := ByteArray.mk (Array.replicate (32 * 1024 * 1024) 7)
  let pb ← sc.send #[big]
  let sh ← sc.shutdown
  tryIO "shutdown while one is pending" do let _ ← sc.shutdown; return ""
  tryIO "send while it is pending" do let _ ← sc.send #[.mk #[1]]; return ""
  let reader ← IO.asTask (prio := .dedicated) do
    let mut total := 0
    repeat
      match ← IO.wait (← c.recv? 65536).result? with
      | some (.ok (some b)) => total := total + b.size
      | _ => break
    return total
  IO.println s!"big send{← wait pb unit}"
  IO.println s!"shutdown{← wait sh unit}"
  IO.println s!"the client read {← IO.ofExcept (← IO.wait reader)} bytes, then end of file"
  tryIO "shutdown after it" do let _ ← sc.shutdown; return ""
  tryIO "send after shutdown" do let _ ← sc.send #[.mk #[1]]; return ""
  tryIO "recv? at end of file" do let p ← c.recv? 100; wait p bytes
  tryIO "recv? at end of file again" do let p ← c.recv? 100; wait p bytes
  tryIO "send to the half-closed peer" do let p ← c.send #[String.toUTF8 "late"]; wait p unit
  tryIO "the peer reads it" do let p ← sc.recv? 100; wait p bytes
  -- a refused connect: a port that was bound, then closed. `d` is bound
  -- while `t` still holds the port, so `d` never gets it (an autobind at
  -- the connect could take the port `t` has just freed, and `d` would
  -- connect to itself, a simultaneous open); `t`'s last use is the
  -- `getSockName` after that bind, where its last reference goes and its
  -- finalizer closes it, before the connect
  let t ← TCP.Socket.new
  t.bind (at_ 0)
  let d ← TCP.Socket.new
  d.bind (at_ 0)
  let closed := (← t.getSockName).port
  tryIO "connect to a closed port" do let p ← d.connect (at_ closed); wait p unit
  tryIO "getPeerName after it" do let _ ← d.getPeerName; return ""
  tryIO "getSockName after it" do let a ← d.getSockName; return s!", port chosen {a.port != 0}"
  tryIO "connect again after it" do let p ← d.connect (at_ closed); wait p unit
  -- a bind to the port in use: reported later
  let s2 ← TCP.Socket.new
  tryIO "bind to the port in use" do s2.bind (at_ port); return ""
  tryIO "getSockName" do let _ ← s2.getSockName; return ""
  tryIO "listen" do s2.listen 5; return ""
  tryIO "connect" do let p ← s2.connect (at_ port); wait p unit
  -- IPv4 socket, IPv6 address
  let s3 ← TCP.Socket.new
  s3.bind (at_ 0)
  tryIO "connect an IPv4 socket to ::1" do
    let p ← s3.connect (.v6 { addr := IPv6Addr.ofParts 0 0 0 0 0 0 0 1, port })
    wait p unit
  -- keep every socket until here
  IO.println s!"sockets kept: {(← s.getSockName).port == port} {(← sc.getSockName).port == port} {(← u.getPeerName.toBaseIO) matches .error _}"
