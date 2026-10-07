-- LB-13, with the waiter on the only standard worker (`LEAN_NUM_THREADS=1`,
-- this case's .env): after `main` returned, a pool task creates another pool
-- task and waits for it. Natively the new task never runs: the one worker is
-- busy with the waiter, and during the shutdown `spawn_worker` returns at
-- once, both at the enqueue and at the wait's raise of the pool's limit, so
-- the process hangs after "released true". Correct: the task runs and the
-- wait returns. leanrs's repro, made deterministic: `main` waits until the
-- task has started, and the task waits until `main` has returned (the
-- shutdown flag that `IO.checkCanceled` reads), so every flag is `true`.

def main : IO Unit := do
  let started ← IO.Promise.new (α := Unit)
  let _ ← IO.asTask (do
    started.resolve ()
    -- until `main` has returned
    while !(← IO.checkCanceled) do
      IO.sleep 10
    let done : Task (Except IO.Error Unit) := Task.pure (.ok ())
    -- a `sync` dependent of a finished task: it runs at once, in this task
    let d ← IO.mapTask (sync := true) (fun _ => do
        let first ← IO.checkCanceled
        IO.eprintln s!"caller {first}"
        -- a pool task, made while the only worker runs this task
        let c ← IO.asTask (do IO.eprintln s!"created {← IO.checkCanceled}")
        let p ← IO.Promise.new (α := Unit)
        -- a `sync` dependent that `p.resolve` runs at once
        let rel ← IO.mapTask (sync := true)
          (fun _ => do IO.eprintln s!"released {← IO.checkCanceled}") p.result!
        p.resolve ()
        let _ ← IO.wait rel
        -- natively this wait never returns
        let _ ← IO.wait c
        pure ()) done
    let _ ← IO.wait d
    pure ())
  let _ ← IO.wait started.result!
  IO.eprintln "main done"
