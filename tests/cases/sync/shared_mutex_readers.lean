import Std.Sync
open Std

-- `BaseSharedMutex`: two readers on threads of their own hold it at once
-- (`tryWrite` fails, `tryRead` succeeds), then a writer waits until both have
-- left. The readers and `main` signal each other with promises, not refs.

def reader (s : BaseSharedMutex) (inside : IO.Promise Unit) (leave : IO.Promise Unit) : IO Unit := do
  s.read
  inside.resolve ()
  let _ ← IO.wait leave.result?
  s.unlockRead

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let s ← BaseSharedMutex.new
  let p1 ← IO.Promise.new (α := Unit)
  let p2 ← IO.Promise.new (α := Unit)
  let leave ← IO.Promise.new (α := Unit)
  let r1 ← IO.asTask (prio := .dedicated) (reader s p1 leave)
  let r2 ← IO.asTask (prio := .dedicated) (reader s p2 leave)
  let _ ← IO.wait p1.result?
  let _ ← IO.wait p2.result?
  let w ← s.tryWrite
  let r ← s.tryRead
  IO.println s!"two readers inside: tryWrite {w}, tryRead {r}"
  s.unlockRead
  let wr ← IO.asTask (prio := .dedicated) do
    s.write
    IO.println "writer entered"
    s.unlockWrite
  IO.sleep ms.toUInt32
  IO.println "readers leave"
  leave.resolve ()
  let _ ← IO.wait wr
  let _ ← IO.wait r1
  let _ ← IO.wait r2
  IO.println s!"after: tryWrite {← s.tryWrite}"
