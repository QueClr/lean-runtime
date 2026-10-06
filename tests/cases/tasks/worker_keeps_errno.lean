/-! A task's `errno` is its thread's, and a pool worker keeps it from one
task to the next (`LEAN_NUM_THREADS=1`: one worker). It shows through a
handle whose sticky error flag is set: `getLine` then reports whatever
`errno` holds (the mechanism of uvsys/errno_after).
- `main` sets the flag of a read-only handle with a failed write (`EBADF`
  on `main`'s thread);
- task A leaves `errno` at `ENOTDIR` (a failed `removeDir` of a file) on
  the worker (not `ENOENT`, whose decoding without a file name crashes
  natively, LB-03);
- `main` sets its own `errno` to `EEXIST` (a failed `createDir`);
- task B, on the same worker, calls `getLine` on the handle: it reports A's
  `ENOTDIR`; `main`'s `getLine` reports `main`'s `EEXIST`.
(Review AR-24 of lean-runtime.) That is native's outcome (`native`): since LB-41 the correct
`getLine` reports only its own error, so B's reads the line and `main`'s reads "". -/
def main : IO Unit := do
  IO.FS.writeFile "e.txt" "line1\n"
  let h ← IO.FS.Handle.mk "e.txt" .read
  try h.putStr "x"; h.flush catch _ => pure ()
  let a ← IO.asTask (do
    try IO.FS.removeDir "e.txt" catch _ => pure ())
  let _ ← IO.wait a
  try IO.FS.createDir "e.txt" catch _ => pure ()
  let b ← IO.asTask (do
    try
      let l ← h.getLine
      return s!"ok {repr l}"
    catch e => return s!"{e}")
  match ← IO.wait b with
  | .ok s => IO.println s!"B's getLine: {s}"
  | .error e => IO.println s!"B failed: {e}"
  try
    let l ← h.getLine
    IO.println s!"main's getLine: ok {repr l}"
  catch e => IO.println s!"main's getLine: {e}"
