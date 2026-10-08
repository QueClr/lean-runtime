import Std.Sync
open Std

-- The owner of a `BaseRecursiveMutex` is an OS thread natively (lean-runtime
-- hunt HSG-01). A task that ends with the mutex locked leaves it locked by
-- its thread: a dedicated task always gets a thread of its own
-- (`spawn_dedicated_worker`), and a pool task runs on an idle pool worker
-- (`enqueue_core` wakes the idle one, and a new worker starts only when none
-- is idle). One line per shape:
-- (a) a dedicated task after a dedicated task: two threads;
-- (b) a dedicated task after a pool task: two threads;
-- (c) a pool task after a pool task, each started while `main` sleeps: the
--     one idle worker runs both;
-- (d) a pool task that a pool task holding the mutex waits for: another
--     worker, since the holder's thread waits in `IO.wait`;
-- (e) a `sync` dependent of a pool task that holds the mutex: it runs on
--     that task's thread when the task finishes.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!.toUInt32
  -- (a)
  let m ← BaseRecursiveMutex.new
  let d1 ← IO.asTask (prio := .dedicated) m.lock
  IO.ofExcept (← IO.wait d1)
  let d2 ← IO.asTask (prio := .dedicated) m.tryLock
  IO.println s!"(a) dedicated after dedicated: tryLock {← IO.ofExcept (← IO.wait d2)}"
  -- (b)
  let m2 ← BaseRecursiveMutex.new
  let p1 ← IO.asTask m2.lock
  IO.ofExcept (← IO.wait p1)
  let d3 ← IO.asTask (prio := .dedicated) m2.tryLock
  IO.println s!"(b) dedicated after pool: tryLock {← IO.ofExcept (← IO.wait d3)}"
  -- (c)
  let m3 ← BaseRecursiveMutex.new
  let p2 ← IO.asTask m3.lock
  IO.sleep ms
  IO.ofExcept (← IO.wait p2)
  let p3 ← IO.asTask m3.tryLock
  IO.sleep ms
  IO.println s!"(c) pool after pool: tryLock {← IO.ofExcept (← IO.wait p3)}"
  -- (d)
  let m4 ← BaseRecursiveMutex.new
  let p4 ← IO.asTask do
    m4.lock
    let q ← IO.asTask m4.tryLock
    IO.ofExcept (← IO.wait q)
  IO.println s!"(d) pool for a pool holder: tryLock {← IO.ofExcept (← IO.wait p4)}"
  -- (e)
  let m5 ← BaseRecursiveMutex.new
  let go ← IO.Promise.new (α := Unit)
  let p5 ← IO.asTask do
    m5.lock
    let _ ← IO.wait go.result?
  let s ← IO.mapTask (sync := true) (fun _ => (m5.tryLock : IO Bool)) p5
  go.resolve ()
  IO.println s!"(e) sync dependent of a pool holder: tryLock {← IO.ofExcept (← IO.wait s)}"
