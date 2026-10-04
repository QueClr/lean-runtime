import Std.Internal.UV
/-! A lookup still in progress when `main` returns, its promise never
awaited: the exit waits for the lookup, as natively (libuv's destructor
joins its thread pool), and drops its answer; the program ends with
`main`'s status. Natively the answer can reach the finalized task manager
and crash the exit (LB-27). The host comes from argv. -/
open Std.Internal.UV

def main (args : List String) : IO Unit := do
  let p ← DNS.getAddrInfo args[0]! "" 0
  IO.println "started"
  -- `p` is kept until here; whether the lookup has finished is a race
  let _ := p
