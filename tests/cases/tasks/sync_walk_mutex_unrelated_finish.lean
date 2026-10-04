import Std.Sync.Mutex

-- A task blocked in `IO.wait r` holds a mutex that `r`'s `sync` dependent
-- needs, so the walk of `r` blocks; an unrelated dedicated task finishes
-- 400 ms in, and its `notify_all` wakes the waiter, which releases the
-- mutex, and the walk goes on: the program ends. A blocking `sync`
-- continuation is documented misuse; native's outcome is followed (not
-- LB-32). (Review of sched-2, RS2-05: without the wake at any finish, a
-- deadlock; probe MutexWalk.)

def main (_args : List String) : IO Unit := do
  let m ← Std.Mutex.new (0 : Nat)
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let s ← IO.mapTask (sync := true) (fun _ => do
    m.atomically (modify (· + 1))
    IO.eprintln "sync dep got the mutex") r
  let w ← IO.asTask (prio := .dedicated) do
    m.atomically do
      let v ← IO.wait r
      IO.eprintln s!"waiter woke holding the mutex: {repr v}"
  let o ← IO.asTask (prio := .dedicated) do
    IO.sleep 400
    IO.eprintln "other task finishes"
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait s
  let _ ← IO.wait o
  IO.eprintln "done"
