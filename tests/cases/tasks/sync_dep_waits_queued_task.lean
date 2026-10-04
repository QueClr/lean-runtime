-- One worker (`LEAN_NUM_THREADS=1`). Task `s` sleeps 50 ms; its `sync`
-- dependent `d`, run in `s`'s walk on `s`'s worker, waits for `q`, an IO
-- task queued at 20 ms (after Lean's panic for a `Task.get` in a `sync`
-- task). Natively a `sync` task's `wait_for` raises no worker limit, and the
-- only worker is busy in the walk, so `q` never runs: a deadlock (misuse:
-- `sync` continuations should not block). A runtime that counts the
-- waiter's worker as free runs `q` on `d`'s stack (sched-4, AR-16 with the
-- head rule of AR-10).

def main (_args : List String) : IO Unit := do
  let r ← IO.mkRef (none : Option (Task (Except IO.Error Unit)))
  let s ← IO.asTask (IO.sleep 50)
  let _d ← IO.mapTask (sync := true) (fun _ => do
    match ← r.get with
    | some q =>
      IO.eprintln "d waits for q"
      let _ ← IO.wait q
      IO.eprintln "d done"
    | none => IO.eprintln "no q") s
  IO.sleep 20
  let q ← IO.asTask (IO.eprintln "q ran")
  r.set (some q)
  IO.sleep 300
  IO.eprintln "main done"
