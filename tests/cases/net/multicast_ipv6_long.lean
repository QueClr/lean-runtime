import Std.Internal.UV
import Std.Net.Addr
open Std.Internal.UV Std.Net

/-! `setMembership` (join, then leave) and `setMulticastInterface` with IPv6
addresses whose text has 15, 16, 17 and 20 characters, on a socket bound
to `[::1]:0`. Natively a text of 16 or more characters aborts the process
(LB-22: a 16-byte buffer, `lean_always_assert`). Each argument is one call
sequence. The judge's probe (net-1, B_Multicast). -/
def v6 (a b c d e f g h : UInt16) : IPAddr := .v6 (IPv6Addr.ofParts a b c d e f g h)

def attempt (what : String) (x : IO Unit) : IO Unit := do
  try x; IO.println s!"{what}: ok" catch e => IO.println s!"{what}: {e}"
  (← IO.getStdout).flush

def main (args : List String) : IO Unit := do
  let u ← UDP.Socket.new
  u.bind (.v6 { addr := IPv6Addr.ofParts 0 0 0 0 0 0 0 1, port := 0 })
  let g7  := v6 0xff02 0 0 0 0 0 0 1                -- ff02::1             (7)
  let g15 := v6 0xff02 0 0 0 0 1 0xff00 0x12        -- ff02::1:ff00:12     (15)
  let g16 := v6 0xff02 0 0 0 0 1 0xff00 0x123       -- ff02::1:ff00:123    (16)
  let g17 := v6 0xff02 0 0 0 0 1 0xff00 0x1234      -- ff02::1:ff00:1234   (17)
  let i15 := v6 0xfe80 0 0 0 0 0 0x1234 0x5678      -- fe80::1234:5678     (15)
  let i20 := v6 0xfe80 0 0 0 0 0x1234 0x5678 0x9abc -- fe80::1234:5678:9abc (20)
  for m in args do
    match m with
    | "g7"  => IO.println s!"text {g7} ({(toString g7).length})";  attempt "join" (u.setMembership g7 none 1);  attempt "leave" (u.setMembership g7 none 0)
    | "g15" => IO.println s!"text {g15} ({(toString g15).length})"; attempt "join" (u.setMembership g15 none 1); attempt "leave" (u.setMembership g15 none 0)
    | "g16" => IO.println s!"text {g16} ({(toString g16).length})"; attempt "join" (u.setMembership g16 none 1); attempt "leave" (u.setMembership g16 none 0)
    | "g17" => IO.println s!"text {g17} ({(toString g17).length})"; attempt "join" (u.setMembership g17 none 1); attempt "leave" (u.setMembership g17 none 0)
    | "i15" => IO.println s!"text {i15} ({(toString i15).length})"; attempt "multicast interface" (u.setMulticastInterface i15)
    | "i20" => IO.println s!"text {i20} ({(toString i20).length})"; attempt "multicast interface" (u.setMulticastInterface i20)
    | "g7i20" => attempt "join g7 on interface i20" (u.setMembership g7 (some i20) 1)
    | "einval" => attempt "membership 2" (u.setMembership g7 none 2)
    | _ => IO.println s!"unknown {m}"
  IO.println "after"
