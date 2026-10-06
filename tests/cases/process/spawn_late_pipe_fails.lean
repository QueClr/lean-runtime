/-! A spawn that fails at its second pipe (LB-44 of `docs/lean-bugs.md`; io bug hunt HIO-04):
`ulimit -n 64`, files opened until `EMFILE`, then three closed. Natively `setup_stdio` throws at
the second `pipe2` (process.cpp 424) and the first pipe's two descriptors stay open for good, so
one descriptor is free after the failed spawn (`native`). The correct outcome: the three are free
again. How many are left free comes from argv. -/
partial def exhaust (acc : Array IO.FS.Handle) : IO (Array IO.FS.Handle) := do
  match ← (IO.FS.Handle.mk "/dev/null" .read).toBaseIO with
  | .ok h => exhaust (acc.push h)
  | .error _ => pure acc

def main (args : List String) : IO Unit := do
  let free := args[0]!.toNat!
  let mut hs ← exhaust #[]
  for _ in [0:free] do
    hs := hs.pop
  try
    let _ ← IO.Process.spawn { cmd := "true", stdin := .piped, stdout := .piped, stderr := .piped }
    IO.println "spawn ok"
  catch e => IO.println s!"spawn failed: {e}"
  let more ← exhaust #[]
  IO.println s!"free after the failed spawn: {more.size} of {free}"
  -- keeps the handles open until here (their only use, or the compiler frees them early)
  IO.println s!"handles kept open: {decide (hs.size > 0)}"
