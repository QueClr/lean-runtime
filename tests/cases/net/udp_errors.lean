import Std.Internal.UV
import Std.Net.Addr
/-! `Std.Internal.UV.UDP.Socket` edge cases, as libuv 1.48 and Linux give
them through Lean 4.34.0's externs: one receive at a time and
`cancelRecv`; an empty datagram (a size of 0 is `recv_zero`); several buffers as one
datagram; an empty array; a datagram too big for IPv4 (`EMSGSIZE`, on the
promise); a connected socket whose peer port is closed (`ECONNREFUSED` on
the next receive); TTL and multicast TTL ranges; option setters on IPv6;
multicast options without joining a group (a membership value other than
0 and 1, leaving a group never joined, interfaces). The addresses come
from argv. -/
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
def dgram (r : ByteArray × Option SocketAddress) : String :=
  s!"{r.1.size} bytes {(String.fromUTF8? r.1).getD "?"} from {match r.2 with | some (.v4 a) => toString a.addr | some (.v6 a) => toString a.addr | none => "none"}"

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let ip6 := (IPv6Addr.ofString args[1]!).get!
  let at_ (port : UInt16) : SocketAddress := .v4 { addr := ip, port }
  let a ← UDP.Socket.new
  a.bind (at_ 0)
  let pa := (← a.getSockName).port
  let b ← UDP.Socket.new
  b.bind (at_ 0)
  let pb := (← b.getSockName).port
  -- one receive at a time
  let r ← b.recv 100
  tryIO "second recv" do let _ ← b.recv 100; return ""
  tryIO "waitReadable while receiving" do let _ ← b.waitReadable; return ""
  b.cancelRecv
  b.cancelRecv
  IO.println s!"cancelled recv resolved: {← r.isResolved}"
  tryIO "empty array" do let p ← a.send #[] (some (at_ pb)); wait p unit
  tryIO "empty datagram" do
    let p ← a.send #[.empty] (some (at_ pb))
    let s ← wait p unit
    let q ← b.recv 100
    return s!"{s}{← wait q dgram}"
  tryIO "three buffers, one datagram" do
    let p ← a.send #[String.toUTF8 "ab", .empty, String.toUTF8 "cd"] (some (at_ pb))
    let s ← wait p unit
    let q ← b.recv 100
    return s!"{s}{← wait q dgram}"
  tryIO "a datagram over 65507 bytes" do
    let p ← a.send #[ByteArray.mk (Array.replicate 70000 1)] (some (at_ pb))
    wait p unit
  -- a connected socket whose peer port is closed: `c` is bound while `t`
  -- still holds its port, so `c` never gets it (an autobind at the connect
  -- could take the port `t` has just freed, and `c` would send to itself);
  -- `t`'s last use is the `getSockName` after that bind, where its last
  -- reference goes and its finalizer closes it, before the connect
  let t ← UDP.Socket.new
  t.bind (at_ 0)
  let c ← UDP.Socket.new
  c.bind (at_ 0)
  let closed := (← t.getSockName).port
  c.connect (at_ closed)
  tryIO "send to a closed port" do let p ← c.send #[String.toUTF8 "x"] none; wait p unit
  tryIO "recv after it" do let q ← c.recv 100; wait q dgram
  -- options
  tryIO "TTL 255" do b.setTTL 255; return ""
  tryIO "TTL 256" do b.setTTL 256; return ""
  tryIO "TTL 2^31" do b.setTTL 0x80000000; return ""
  tryIO "multicast TTL 0" do b.setMulticastTTL 0; return ""
  tryIO "multicast TTL 256" do b.setMulticastTTL 256; return ""
  tryIO "multicast loop" do b.setMulticastLoop true; return ""
  tryIO "multicast interface" do b.setMulticastInterface (.v4 ip); return ""
  tryIO "multicast interface IPv6 on an IPv4 socket" do b.setMulticastInterface (.v6 ip6); return ""
  let g := IPAddr.v4 (IPv4Addr.ofParts 239 1 2 3)
  tryIO "membership 2" do b.setMembership g none 2; return ""
  tryIO "leave a group never joined" do b.setMembership g none 0; return ""
  tryIO "leave with an IPv6 interface" do b.setMembership g (some (.v6 ip6)) 0; return ""
  tryIO "IPv6 group on an IPv4 socket" do b.setMembership (.v6 (IPv6Addr.ofParts 0xff02 0 0 0 0 0 0 1)) none 0; return ""
  -- IPv6
  let v ← UDP.Socket.new
  v.bind (.v6 { addr := ip6, port := 0 })
  tryIO "IPv6 TTL" do v.setTTL 3; return ""
  tryIO "IPv6 multicast TTL" do v.setMulticastTTL 3; return ""
  tryIO "IPv6 multicast loop" do v.setMulticastLoop false; return ""
  tryIO "IPv6 multicast interface" do v.setMulticastInterface (.v6 ip6); return ""
  tryIO "IPv6 leave a group never joined" do v.setMembership (.v6 (IPv6Addr.ofParts 0xff02 0 0 0 0 0 0 1)) none 0; return ""
  tryIO "IPv6 group with an IPv4 interface" do v.setMembership (.v6 (IPv6Addr.ofParts 0xff02 0 0 0 0 0 0 1)) (some (.v4 ip)) 0; return ""
  tryIO "IPv6 datagram to itself" do
    let p ← v.send #[String.toUTF8 "six"] (some (← v.getSockName))
    let s ← wait p unit
    let q ← v.recv 100
    return s!"{s}{← wait q dgram}"
  IO.println s!"sockets kept: {(← a.getSockName).port == pa} {(← b.getSockName).port == pb}"
