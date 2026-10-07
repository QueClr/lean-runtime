-- `main` waits for `u`, a dependent of `s2`, a `sync := true` dependent of
-- the pure bind task `r`, whose function returns an unfinished pure task.
-- Natively `s2` runs once `r` has finished, so its `Task.get` of `r`
-- (inside `Task.map`) does not wait: `3`, nothing on stderr. Before the
-- fix of review HL2-03 the single-thread scheduler ran `r` on `main`'s
-- stack, then `s2` before `r` had finished (`wait`'s chain was stale), and
-- `s2` printed "`Task.get` called from a `(sync := true)` task".

def main (args : List String) : IO Unit := do
  let k := args.length
  let r : Task Nat := (Task.pure k).bind fun j => Task.spawn fun _ => j + 1
  let s2 : Task Nat := r.map (sync := true) fun x => x + 1
  let u : Task Nat := s2.map fun x => x + 1
  IO.println s!"{u.get}"
