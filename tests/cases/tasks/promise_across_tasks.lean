-- A task on a thread of its own waits for a promise that `main` resolves
-- after a sleep (meanwhile it is running, blocked); a promise dropped
-- unresolved resolves its `result?` with `none` (Lean's
-- `deactivate_promise`).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let p ← IO.Promise.new (α := Nat)
  let t ← IO.asTask (prio := .dedicated) do
    let v ← IO.wait p.result!
    return v + 1
  IO.sleep ms.toUInt32
  IO.println s!"task finished before resolve: {← IO.hasFinished t}"
  p.resolve 41
  match ← IO.wait t with
  | .ok v => IO.println s!"task got {v}"
  | .error e => IO.println s!"error {e}"
  let q ← IO.Promise.new (α := Nat)
  let r := q.result?
  IO.println s!"dropped promise: {(← IO.wait r).isNone}"
