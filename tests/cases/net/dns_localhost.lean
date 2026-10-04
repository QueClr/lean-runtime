import Std.Internal.UV
import Std.Net.Addr
/-! `Std.Internal.UV.DNS` on names the host resolves locally: `localhost`
(from `/etc/hosts`) and numeric hosts, through glibc's `getaddrinfo` with
libuv's hints (socket type 0, so one entry per socket type), each family;
glibc's errors as libuv codes; the checks that fail at once (a byte Lean
does not allow, an empty host, a host of 256 bytes); `getNameInfo` of
loopback addresses with numeric services. The names come from argv. -/
open Std.Internal.UV Std.Net

def gai (h s : String) (f : UInt8) : IO Unit := do
  try
    let p ← DNS.getAddrInfo h s f
    match ← IO.wait p.result? with
    | none => IO.println s!"{repr h} {repr s} {f}: dropped"
    | some (.ok a) => IO.println s!"{repr h} {repr s} {f}: {a.map toString}"
    | some (.error e) => IO.println s!"{repr h} {repr s} {f}: then {e}"
  catch e => IO.println s!"{repr h} {repr s} {f}: {e}"

def gni (a : SocketAddress) : IO Unit := do
  let shown := match a with
    | .v4 x => s!"{x.addr} {x.port}"
    | .v6 x => s!"{x.addr} {x.port}"
  try
    let p ← DNS.getNameInfo a
    match ← IO.wait p.result? with
    | none => IO.println s!"name of {shown}: dropped"
    | some (.ok (h, s)) => IO.println s!"name of {shown}: {repr h} {repr s}"
    | some (.error e) => IO.println s!"name of {shown}: then {e}"
  catch e => IO.println s!"name of {shown}: {e}"

def main (args : List String) : IO Unit := do
  let host := args[0]!
  let v4 := args[1]!
  let v6 := args[2]!
  for h in [host, host.toUpper, host ++ ".", v4, v6, "1.2.3"] do
    for s in ["", "0", "8080"] do
      for f in [0, 1, 2, 3] do
        gai h s f
  gai v4 "no-such-service-here" 0
  gai "" "" 0
  gai "a b" "" 0
  gai host "é" 0
  gai (String.ofList (List.replicate 256 'a')) "" 0
  for p in [0, 65535, 49999] do
    gni (.v4 { addr := (IPv4Addr.ofString v4).get!, port := p })
    gni (.v6 { addr := (IPv6Addr.ofString v6).get!, port := p })
