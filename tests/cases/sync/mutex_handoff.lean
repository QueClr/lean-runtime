import Std.Sync
open Std

-- `BaseMutex` between `main` and a task on a thread of its own
-- (`Task.Priority.dedicated`): `tryLock` fails while the lock is held, also
-- by the thread that holds it (glibc's mutex is not recursive); the task
-- blocks on the lock `main` holds across a sleep, and gets it once `main`
-- unlocks. The log is written in an order the sleep and the lock fix.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let m ← BaseMutex.new
  let log ← IO.mkRef (#[] : Array String)
  m.lock
  let t ← IO.asTask (prio := .dedicated) do
    log.modify (·.push s!"task tries: {← m.tryLock}")
    m.lock
    log.modify (·.push "task has the lock")
    m.unlock
  IO.sleep ms.toUInt32
  log.modify (·.push s!"main tries its own lock: {← m.tryLock}")
  log.modify (·.push "main unlocks")
  m.unlock
  let _ ← IO.wait t
  for l in ← log.get do IO.println l
