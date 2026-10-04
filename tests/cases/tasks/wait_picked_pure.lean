-- One worker. A dedicated task ticks until `main` stops it; `a` (an IO
-- task) and then `t` (a pure task) are queued, and `main` waits for `t`.
-- Natively the worker runs `a`, then `t`, and `main` goes on. Here `t` is
-- not the queue's head when `main` waits, so `main` blocks; a worker that
-- reaches `t` only marks it started (the pure-task rule), and that must
-- wake `main`, which needs it: otherwise the ticking keeps the scheduler
-- from its last resort, and the program hangs (sched-3, review AR-10).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let stop ← IO.mkRef false
  let ticker ← IO.asTask (prio := .dedicated) (do
    while !(← stop.get) do IO.sleep 20)
  let a ← IO.asTask (IO.println "a ran")
  let t := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  IO.println s!"t = {t.get}"
  stop.set true
  let _ ← IO.wait ticker
  let _ ← IO.wait a
  IO.println "main done"
