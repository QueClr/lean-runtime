import Std.Sync
open Std

-- `BaseRecursiveMutex`: `main` takes it twice; a task on a thread of its
-- own fails `tryLock` and blocks in `lock` until `main` has unlocked twice.
-- The task resolves `tried` after its `tryLock` line, and `main` waits for
-- it before its first unlock, so the lines' order does not depend on when
-- the task's new thread starts (review AR-44).

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let r ← BaseRecursiveMutex.new
  r.lock
  IO.println s!"main tryLock again: {← r.tryLock}"
  let tried ← IO.Promise.new (α := Unit)
  let t ← IO.asTask (prio := .dedicated) do
    IO.println s!"task tryLock: {← r.tryLock}"
    tried.resolve ()
    r.lock
    IO.println "task has the lock"
    r.unlock
  let _ ← IO.wait tried.result?
  IO.sleep ms.toUInt32
  r.unlock
  IO.println "main unlocked once"
  IO.sleep ms.toUInt32
  IO.println "main unlocks the second time"
  r.unlock
  let _ ← IO.wait t
