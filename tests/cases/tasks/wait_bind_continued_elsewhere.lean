-- Four workers (`.env`). `IO.wait` on a pure `Task.bind` task `s` that
-- runs on a thread of its own (here: `IO.waitAny` starts it on a context
-- of its own) and then continues as an unfinished pure task (its function
-- returns a `Task.spawn`). A dedicated ticker sleeps in a loop until
-- `main` is done, so something always sleeps. `IO.waitAny` returns `fast`
-- first (`s` waits 300 ms for `y`), but only that it returned is printed.
-- Natively three lines, status 0. Before the fix of review HL2-01 the
-- single-thread scheduler hung in `IO.wait s`: `main` blocked on `s` while
-- it ran, and nothing woke it when `s` came to wait for the spawned task.

def slowGet (y : Task (Except IO.Error Unit)) (k : Nat) : Nat :=
  match y.get with
  | .ok _ => k + 1
  | .error _ => k

def main (args : List String) : IO Unit := do
  let k := args.length
  let done ← IO.mkRef false
  let ticker ← IO.asTask (prio := .dedicated) do
    while !(← done.get) do IO.sleep 50
  let y ← IO.asTask (IO.sleep 300)
  -- s's function blocks in `y.get`, then returns an unfinished pure task
  let s : Task Nat := (Task.pure k).bind fun j =>
    let n := slowGet y j
    Task.spawn fun _ => n + 41
  let fast : Task Nat := Task.spawn fun _ => k + 7
  let _ ← IO.waitAny [s, fast]
  IO.println "waitAny returned"
  (← IO.getStdout).flush
  let v ← IO.wait s
  IO.println s!"s: {v}"
  done.set true
  let _ ← IO.wait ticker
  IO.println "done"
