/-! `IO.getTID` in tasks: which tasks share an OS thread (review AR-37 of
lean-runtime, lean2rr's RS5-04). Natively (`task_manager`, `object.cpp`):
- a pool task runs on a standard worker, which stays alive and takes the
  next pool task once idle; a new worker starts only when none is idle
  (`enqueue_core`), here when `t1` blocks in `IO.wait`;
- a dedicated task always runs on a new thread of its own
  (`spawn_dedicated_worker`);
- a `sync` dependent runs on the thread that finishes its source; for a
  promise's task, the thread that resolves it (`handle_finished`).
So the first pool task and a dedicated task that follows it have different
ids, though the first has finished. Only relations are printed: the ids
change from run to run. -/
def main : IO Unit := do
  let mt ← IO.getTID
  -- one after the other: pool, dedicated, pool, dedicated
  let a ← IO.wait (← IO.asTask IO.getTID)
  let d ← IO.wait (← IO.asTask (prio := .dedicated) IO.getTID)
  let b ← IO.wait (← IO.asTask IO.getTID)
  let e ← IO.wait (← IO.asTask (prio := .dedicated) IO.getTID)
  -- alive at once: two pool tasks and a dedicated one wait for a promise
  let p ← IO.Promise.new (α := Unit)
  let t1 ← IO.asTask (do let _ ← IO.wait p.result?; IO.getTID)
  let t2 ← IO.asTask (prio := .dedicated) (do let _ ← IO.wait p.result?; IO.getTID)
  let t3 ← IO.asTask (do let _ ← IO.wait p.result?; IO.getTID)
  let s1 ← IO.mapTask (sync := true) (fun _ => IO.getTID) t1
  let sp ← IO.mapTask (sync := true) (fun _ => IO.getTID) p.result?
  IO.sleep 100
  p.resolve ()
  match a, d, b, e, ← IO.wait t1, ← IO.wait t2, ← IO.wait t3, ← IO.wait s1, ← IO.wait sp with
  | .ok a, .ok d, .ok b, .ok e, .ok t1, .ok t2, .ok t3, .ok s1, .ok sp =>
    IO.println s!"tasks differ from main: {a != mt && d != mt && t1 != mt && t2 != mt && t3 != mt}"
    IO.println s!"sequential pool vs dedicated differ: {a != d}"
    IO.println s!"sequential pool tasks share the idle worker: {a == b}"
    IO.println s!"dedicated tasks never share a thread: {d != e && d != t2 && e != t2}"
    IO.println s!"a dedicated task is on no pool worker: {d != b && e != a && t2 != t1 && t2 != t3}"
    IO.println s!"a later pool task takes the idle worker: {t1 == a}"
    IO.println s!"pool tasks alive at once differ: {t1 != t3}"
    IO.println s!"sync dependent on its source's thread: {s1 == t1}"
    IO.println s!"sync dependent of a promise on the resolving thread: {sp == mt}"
  | _, _, _, _, _, _, _, _, _ => IO.println "error"
