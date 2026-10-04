import Std.Sync.Mutex

-- `sync_walk_mutex_unrelated_finish` with the unrelated task not referenced
-- (`let _ ← IO.asTask`): natively its last reference goes when it ends, so
-- its finish deletes it (`m_deleted`) without `resolve_core`, and notifies
-- nobody: the waiter holding the mutex never wakes, a deadlock, natively and
-- here (a blocking `sync` continuation, documented misuse; not LB-32).
-- (Review of sched-2, RS2-08; probe MutexWalkUnref.)

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
  let _ ← IO.asTask (prio := .dedicated) do
    IO.sleep 400
    IO.eprintln "other task finishes"
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait s
  IO.eprintln "done"
