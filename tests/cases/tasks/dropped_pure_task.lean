-- A pure task the program drops while it is still queued is deleted and never
-- runs, so a runaway one does not keep the process alive. Native 4.33.0,
-- 4.34.0-rc1 and 4.34.0: exits in 20 of 20 runs (leanrs exit probe 3,
-- PExitPureDropped). `src/runtime/object.cpp`: a queued task whose last
-- reference goes is deactivated before a worker starts it.
--
-- `IO.hasFinished t` pins the task's creation here: a task used only on a
-- branch the arguments never take would be floated into that branch and
-- never created at all.

partial def spin (x acc : UInt64) : UInt64 :=
  if x == 0 then acc else spin (x * 6364136223846793005 + 1442695040888963407) (acc + 1)

def main (args : List String) : IO Unit := do
  let s := args.head!.toNat!.toUInt64 ||| 1
  let t := Task.spawn fun _ => spin s 0
  let f ← IO.hasFinished t
  IO.eprintln s!"main done {f}"
