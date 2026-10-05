-- `IO.waitAny` returns the first task of its list to finish, not the
-- first in the list: two IO tasks run at once on two workers
-- (`LEAN_NUM_THREADS=2`), the slow one first in the list. `waitAny` wakes
-- when the fast one finishes, while the slow one still sleeps.

def main (args : List String) : IO Unit := do
  let slowMs := args[0]!.toNat!
  let fastMs := args[1]!.toNat!
  let slow ← IO.asTask do
    IO.sleep slowMs.toUInt32
    return "slow"
  let fast ← IO.asTask do
    IO.sleep fastMs.toUInt32
    return "fast"
  match ← IO.waitAny [slow, fast] with
  | .ok s => IO.println s!"waitAny: {s}"
  | .error e => IO.println s!"error: {e}"
  IO.println s!"slow finished then: {← IO.hasFinished slow}"
  match ← IO.wait slow with
  | .ok s => IO.println s!"then {s}"
  | .error e => IO.println s!"error: {e}"
