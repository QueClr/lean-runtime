-- A loop polling an `IO.Ref` with `IO.sleep 1` in its body, waiting for a flag
-- an IO task sets, terminates natively in every run (20 of 20). Polling with
-- no sleep at all is natively nondeterministic: `lean_st_ref_get`'s
-- multi-threaded path can lose a concurrent set (refinement B), so it is not
-- a test.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let flag ← IO.mkRef false
  let _ ← IO.asTask do
    IO.sleep ms.toUInt32
    flag.set true
  while !(← flag.get) do
    IO.sleep 1
  IO.println "flag seen"
