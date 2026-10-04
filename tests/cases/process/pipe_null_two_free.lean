/-! LB-17's cost: a `null` stream after a piped one needs one free descriptor more than natively.
Natively the parent makes the pipe (two descriptors) and only the forked child opens
`/dev/null`, after it has closed the pipe's other end (process.cpp, `spawn`). Both translators
open `/dev/null` in the parent, close-on-exec, so that a failure is the spawn's error (LB-17);
that needs a third free descriptor. Run under `ulimit -n 64`: the parent opens handles until
`EMFILE`, closes two, and spawns with `stdout := .piped, stderr := .null`. Native: the child
runs. Both translators (the alternative outcome): the spawn fails with `EMFILE`. -/
partial def exhaust (acc : Array IO.FS.Handle) : IO (Array IO.FS.Handle) := do
  match ← (IO.FS.Handle.mk "/dev/null" .read).toBaseIO with
  | .ok h => exhaust (acc.push h)
  | .error _ => pure acc

def main : IO Unit := do
  -- `pop` drops a handle, which closes it: exactly two descriptors are free
  let hs := (← exhaust #[]).pop.pop
  try
    let c ← IO.Process.spawn { cmd := "sh", args := #["-c", "echo out; echo err >&2"], stdout := .piped, stderr := .null }
    let out ← c.stdout.readToEnd
    IO.println s!"child: stdout {repr out}, exit {← c.wait}"
  catch e => IO.println s!"spawn failed: {e}"
  -- keeps the handles open until here
  IO.println s!"handles kept open: {decide (hs.size > 0)}"
