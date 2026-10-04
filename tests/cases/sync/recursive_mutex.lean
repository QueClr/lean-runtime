import Std.Sync
open Std

-- `BaseRecursiveMutex`: `main` takes it twice; a task on a thread of its
-- own fails `tryLock` and blocks in `lock` until `main` has unlocked twice.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let r ← BaseRecursiveMutex.new
  r.lock
  IO.println s!"main tryLock again: {← r.tryLock}"
  let t ← IO.asTask (prio := .dedicated) do
    IO.println s!"task tryLock: {← r.tryLock}"
    r.lock
    IO.println "task has the lock"
    r.unlock
  IO.sleep ms.toUInt32
  r.unlock
  IO.println "main unlocked once"
  IO.sleep ms.toUInt32
  IO.println "main unlocks the second time"
  r.unlock
  let _ ← IO.wait t
