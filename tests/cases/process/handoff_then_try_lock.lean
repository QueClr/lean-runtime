import Std.Sync
open Std

/-! Each of `Std.Sync`'s try functions right after a pipe handle's drop
whose `fclose` blocks natively until the child reads (about 0.5 s), while a
dedicated task takes the lock at 150 ms: natively the lock is taken by
then, and the try fails. Before review HR-03 (fixes-14) the single-thread
scheduler's try functions made no writers point, unlike their blocking
counterparts, so in a translator whose drop hands the bytes to a writer
thread, a try right after the drop (with no other writers point between,
the drain's end included) took the lock before the task could. -/

def round (name : String) (take : IO Unit) (attempt : IO Bool) : IO Unit := do
  let started ← IO.Promise.new (α := Unit)
  let taker ← IO.asTask (prio := .dedicated) do
    started.resolve ()
    IO.sleep 150
    take
  let _ ← IO.wait started.result?
  let child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", "sleep 0.5; cat > /dev/null"], stdin := .piped }
  let (stdin, child) ← child.takeStdin
  stdin.write (ByteArray.mk (Array.replicate 65536 120))
  stdin.flush
  stdin.putStr "x"
  -- `stdin`'s last use: its finalizer closes it here, before the try
  let got ← attempt
  IO.println s!"{name}: {got}"
  let _ ← IO.wait taker
  let _ ← child.wait

def main : IO Unit := do
  let m ← BaseMutex.new
  round "BaseMutex.tryLock" m.lock m.tryLock
  let r ← BaseRecursiveMutex.new
  round "BaseRecursiveMutex.tryLock" r.lock r.tryLock
  let s ← BaseSharedMutex.new
  round "BaseSharedMutex.tryWrite" s.write s.tryWrite
  let w ← BaseSharedMutex.new
  round "BaseSharedMutex.tryRead" w.write w.tryRead
