-- A pure task still referenced when main returns (here from a module-level
-- reference) runs at exit, so a runaway one keeps the process alive (leanrs
-- exit probe 2, PExitPureKept).

partial def spin (x acc : UInt64) : UInt64 :=
  if x == 0 then acc else spin (x * 6364136223846793005 + 1442695040888963407) (acc + 1)

initialize keep : IO.Ref (Option (Task UInt64)) ← IO.mkRef none

def main (args : List String) : IO Unit := do
  let s := args.head!.toNat!.toUInt64 ||| 1
  let t := Task.spawn fun _ => spin s 0
  keep.set (some t)
  IO.eprintln "main done"
