import Std.Sync.Mutex

-- `sync_walk_mutex_unrelated_finish` without the unrelated task: the waiter
-- of `r` holds the mutex that `r`'s `sync` dependent needs, and nothing
-- else ever finishes, so nothing wakes the waiter: a deadlock, natively and
-- here (a blocking `sync` continuation, documented misuse; not LB-32).

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
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln "resolved"
  let _ ← IO.wait w
  let _ ← IO.wait s
  IO.eprintln "done"
