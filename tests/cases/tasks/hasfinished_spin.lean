-- A loop polling `IO.hasFinished` on a task terminates natively in every run:
-- task-state queries must let the task run (a polling yield point).

def main (args : List String) : IO Unit := do
  let n := args.head!.toNat!
  let t ← IO.asTask do
    let mut s := 0
    for i in [0:n] do
      s := s + i
    return s
  while !(← IO.hasFinished t) do
    pure ()
  match t.get with
  | .ok s => IO.println s!"finished: {s}"
  | .error e => IO.println s!"error: {e}"
