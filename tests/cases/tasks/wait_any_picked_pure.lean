-- One worker. A dedicated task ticks until `main` stops it; `main` waits
-- for either of two pure tasks with `IO.waitAny`. Natively the worker runs
-- `t1` first, and `waitAny` returns its value. Here `main` blocks while a
-- worker marks both started (the pure-task rule); `waitAny` needs them, so
-- they must start on contexts of their own: otherwise the ticking keeps
-- the scheduler from its last resort, and the program hangs (sched-3,
-- review AR-10, waitAny).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let stop ← IO.mkRef false
  let ticker ← IO.asTask (prio := .dedicated) (do
    while !(← stop.get) do IO.sleep 20)
  let t1 := Task.spawn fun _ => (List.range n).foldl (· + ·) 0
  let t2 := Task.spawn fun _ => (List.range (n + 1)).foldl (· + ·) 0
  let r ← IO.waitAny [t1, t2]
  IO.println s!"waitAny: {r}"
  stop.set true
  let _ ← IO.wait ticker
  IO.println "main done"
