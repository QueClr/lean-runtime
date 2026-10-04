import Std.Net.Addr
/-! `Std.Net.interfaceAddresses`, keeping the entries of the interface whose
name comes from argv (the loopback interface): its addresses, netmasks,
the loopback flag and the hardware address. -/
open Std.Net

def main (args : List String) : IO Unit := do
  let all ← interfaceAddresses
  let mine := all.filter (·.name == args[0]!)
  IO.println s!"{mine.size} entries for {args[0]!}"
  for i in mine do
    IO.println s!"{i.name} loopback {i.isLoopback} {i.address} mask {i.netMask} hw {i.physicalAddress.octets.toArray}"
