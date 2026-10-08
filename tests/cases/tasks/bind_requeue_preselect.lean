-- One worker (`LEAN_NUM_THREADS=1`, this case's .env). `t0`, a pool task,
-- waits for a promise all along (natively its wait raises the worker limit
-- by one, so the queue's tasks get one worker); `y` and `x` are dedicated
-- tasks that wait for promises. `main` queues the bind task `s`, hands it
-- to `y` and `x` (resolving their promises), and polls `s` until it has
-- finished. `s`'s function returns a task that waits for the promise `p2`,
-- so `s` waits for it. `y` waits for `s`. `x` queues `z` and waits for it;
-- `z` resolves `p2`, which queues `s`'s continuation, sleeps 20 ms on the
-- one worker, and then looks whether `s` has finished. Natively the
-- continuation needs that worker, so it runs only once `z` has ended:
-- "s finished before z's end: false". Review of lean-runtime's fixes-17
-- (single-thread scheduler): the polling threshold started `s` on a
-- context of its own, `y` ran `s`'s function first, on its stack, and that
-- context, when it first ran, began the continuation while `z` held the
-- worker ("true"). No `ST.Ref` is read before `z`'s end: the first
-- reference read is a polling point of that scheduler, where the context
-- ran first.

def main : IO Unit := do
  let p0 ← IO.Promise.new (α := Unit)
  let t0 ← IO.asTask (do let _ ← IO.wait p0.result?; pure ())
  -- t0 starts and waits
  IO.sleep 5
  let p2 ← IO.Promise.new (α := Unit)
  let ps ← IO.Promise.new (α := Task (Except IO.Error (Option Unit)))
  let y ← IO.asTask (prio := .dedicated) do
    let some s ← IO.wait ps.result? | pure ()
    let _ ← IO.wait s
  let px ← IO.Promise.new (α := Task (Except IO.Error (Option Unit)))
  let x ← IO.asTask (prio := .dedicated) do
    let some s ← IO.wait px.result? | pure false
    let z ← IO.asTask do
      p2.resolve ()
      IO.sleep 20
      IO.hasFinished s
    IO.ofExcept (← IO.wait z)
  -- y and x start and wait
  IO.sleep 5
  let s ← IO.bindTask (Task.pure ()) fun _ => pure (p2.result?.map (sync := true) Except.ok)
  ps.resolve s
  px.resolve s
  while !(← IO.hasFinished s) do pure ()
  let _ ← IO.wait y
  let early ← IO.ofExcept (← IO.wait x)
  p0.resolve ()
  let _ ← IO.wait t0
  IO.println s!"s finished before z's end: {early}"
