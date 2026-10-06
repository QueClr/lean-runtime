/-! `IO.getRandomBytes` of a size whose array would overflow (LB-43 of `docs/lean-bugs.md`; io bug
hunt HIO-05), with one descriptor free (`ulimit -n 64`, files opened until `EMFILE`, then one
closed). Lean opens `/dev/urandom` (an open error comes first, AR-1), then fails with `ENOMEM`
and leaves the descriptor open for good (io.cpp 878-885), so the next call fails at the open,
and so does a small request (`native`). The correct outcome: each such call fails with `ENOMEM`,
and the small request gets its bytes. The sizes and the number left free come from argv. -/
partial def exhaust (acc : Array IO.FS.Handle) : IO (Array IO.FS.Handle) := do
  match ← (IO.FS.Handle.mk "/dev/null" .read).toBaseIO with
  | .ok h => exhaust (acc.push h)
  | .error _ => pure acc

def attempt (n : USize) : IO String := do
  match ← (IO.getRandomBytes n).toBaseIO with
  | .ok b => pure s!"{b.size} bytes"
  | .error e => pure s!"error: {e}"

def main (args : List String) : IO Unit := do
  let huge := args[0]!.toNat!.toUSize
  let small := args[1]!.toNat!.toUSize
  let free := args[2]!.toNat!
  -- the pops in a loop of `IO`, so they come before the calls (a pure `pop` would be floated to
  -- the last use of `hs`)
  let mut hs ← exhaust #[]
  for _ in [0:free] do
    hs := hs.pop
  IO.println s!"getRandomBytes {huge}: {← attempt huge}"
  IO.println s!"getRandomBytes {huge}: {← attempt huge}"
  IO.println s!"getRandomBytes {small}: {← attempt small}"
  -- keeps the handles open until here (their only use, or the compiler frees them early)
  IO.println s!"handles kept open: {decide (hs.size > 0)}"
