-- More pool tasks than workers, waiting on each other: task i waits for
-- promise i, which task i + 1 resolves once its own wait is over; the last
-- task resolves its predecessor's at once. With `LEAN_NUM_THREADS=2` the
-- first two tasks wait on both workers, and each wait in a pool task raises
-- the pool's limit by one and starts a worker (`task_manager::wait_for`,
-- `object.cpp` 1024-1046; `Task.get`'s documentation), so every task gets
-- a worker and the chain unwinds from the last task to the first. Each line
-- is printed before the resolution it reports, so the order has no race.

/-- Resolve promise `i` with `v`. -/
def resolveAt (ps : Array (IO.Promise Nat)) (i v : Nat) : BaseIO Unit := do
  if h : i < ps.size then ps[i].resolve v

/-- Wait for promise `i`. -/
def waitAt (ps : Array (IO.Promise Nat)) (i : Nat) : BaseIO Nat := do
  if h : i < ps.size then
    return (← IO.wait ps[i].result?).getD 0
  else
    return 0

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let ps ← (List.range n).toArray.mapM fun _ => IO.Promise.new (α := Nat)
  let mut ts := #[]
  for i in [0:n] do
    let t ← IO.asTask do
      if i + 1 == n then
        IO.println s!"task {i} resolves task {i - 1}'s promise"
        resolveAt ps (i - 1) 1
        return 1
      else
        let v ← waitAt ps i
        IO.println s!"task {i} got {v}"
        if i > 0 then
          resolveAt ps (i - 1) (v + 1)
        return v + 1
    ts := ts.push t
  match ts[0]? with
  | some t =>
    match ← IO.wait t with
    | .ok v => IO.println s!"main got {v}"
    | .error e => IO.println s!"error: {e}"
  | none => IO.println "no task"
