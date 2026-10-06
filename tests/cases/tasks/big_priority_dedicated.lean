-- A task priority above `Task.Priority.max` (8) makes a dedicated task, whatever
-- its size (Init/Core.lean: "Tasks with a priority greater than
-- `Task.Priority.max` are scheduled on dedicated threads"). Native Lean cuts the
-- priority to a C `unsigned` (LB-39 of docs/lean-bugs.md): 2^32 - 1 is
-- `LEAN_SYNC_PRIO`, so the task runs at once inside the spawn, on the spawning
-- thread, as a `sync` task; 2^32 is priority 0, a pool task.
--
-- Part 1, for `IO.asTask`, `Task.spawn` and `Task.map` (of a finished task) at
-- each priority of the arguments: the task returns the value of a promise `go`,
-- which `main` resolves right after the spawn returns, or a rescuer after a
-- second. A dedicated task waits for `go`, so it has not finished when the spawn
-- returns, and it gets `main`'s value. A task run inside the spawn prints the
-- `sync` task panic and gets the rescuer's.
--
-- Part 2, `IO.asTask`, and `IO.mapTask` and `IO.bindTask` of a finished task, while
-- the one pool worker (`LEAN_NUM_THREADS=1`) runs `busy`, which polls until `free`
-- is resolved: a dedicated task runs while the worker is busy and frees it; a pool
-- task waits until the rescuer frees it after a second. Natively a task at
-- 2^32 - 1 runs inside the spawn, on `main`'s thread, so it also sees the worker
-- busy.

def seen (r : Option String) : String :=
  match r with
  | some s => s
  | none => "dropped"

/-- A dedicated task that resolves `p` with "rescuer" after a second. -/
def rescue (p : IO.Promise String) : IO Unit := do
  let _ ← IO.asTask (prio := .dedicated) (do IO.sleep 1000; p.resolve "rescuer")

def part1 (a : String) : IO Unit := do
  let p := a.toNat!
  let go ← IO.Promise.new
  rescue go
  let t ← IO.asTask (prio := p) (return seen (← IO.wait go.result?))
  IO.println s!"asTask {a}: finished when the spawn returned: {← IO.hasFinished t}"
  go.resolve "main"
  match ← IO.wait t with
  | .ok s => IO.println s!"asTask {a}: the task saw {s}"
  | .error e => IO.println s!"asTask {a}: error {e}"
  let go ← IO.Promise.new
  rescue go
  let t := Task.spawn (prio := p) fun _ => seen go.result?.get
  IO.println s!"spawn {a}: finished when the spawn returned: {← IO.hasFinished t}"
  go.resolve "main"
  IO.println s!"spawn {a}: the task saw {t.get}"
  let go ← IO.Promise.new
  rescue go
  let t := (Task.pure p).map (prio := p) fun _ => seen go.result?.get
  IO.println s!"map {a}: finished when the spawn returned: {← IO.hasFinished t}"
  go.resolve "main"
  IO.println s!"map {a}: the task saw {t.get}"

/-- Part 2 for one spawner: `spawn done free` makes the task at the priority under test while the
one pool worker runs `busy`, which polls until `free` is resolved, then resolves `done`. -/
def busyCase (label : String)
    (spawn : IO.Promise Unit → IO.Promise String → BaseIO (Task (Except IO.Error Bool))) :
    IO Unit := do
  let free ← IO.Promise.new
  let started ← IO.Promise.new
  let done ← IO.Promise.new
  let busy ← IO.asTask (do
    started.resolve ()
    while !(← IO.hasFinished free.result?) do IO.sleep 10
    done.resolve ())
  let _ ← IO.wait started.result?
  rescue free
  let t ← spawn done free
  match ← IO.wait t with
  | .ok b => IO.println s!"{label}: the task ran while the worker was busy: {b}"
  | .error e => IO.println s!"{label}: error {e}"
  IO.println s!"{label}: the worker was freed by {seen (← IO.wait free.result?)}"
  let _ ← IO.wait busy

/-- The task of part 2: whether `busy` still runs; then it frees the worker. -/
def check (done : IO.Promise Unit) (free : IO.Promise String) : IO Bool := do
  let ranWhileBusy := !(← IO.hasFinished done.result?)
  free.resolve "the task"
  return ranWhileBusy

def part2 (a : String) : IO Unit := do
  let p := a.toNat!
  busyCase s!"busy asTask {a}" fun done free => IO.asTask (prio := p) (check done free)
  busyCase s!"busy mapTask {a}" fun done free =>
    IO.mapTask (prio := p) (fun _ => check done free) (Task.pure p)
  busyCase s!"busy bindTask {a}" fun done free =>
    IO.bindTask (prio := p) (Task.pure p) fun _ => do
      let b ← check done free
      return Task.pure (.ok b)

def main (args : List String) : IO Unit := do
  for a in args do part1 a
  for a in args do part2 a
