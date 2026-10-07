/-! A promise resolved twice keeps its first value (Init/System/Promise.lean:
"Only the first call to this function has an effect"). `main` drops a pipe
handle whose last byte the pipe cannot take yet: natively its `fclose`
blocks until the child reads, about 1 s. Task B resolves the promise with 2
at 300 ms; then `main` resolves it with 1, which has no effect. Natively:
"B sees (some 2)", "main sees (some 2)". In a translator whose drop hands
the bytes to a writer thread, the writers point at the drain's end (or at
the resolution) lets B run first; a glue that tested the promise before
that point stored 1 over 2 (lean2rr's HR-01, fixed in its glue). -/
def main : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let b ← IO.asTask (do
    IO.sleep 300
    p.resolve 2
    let v ← IO.wait p.result?
    IO.println s!"B sees {v}")
  let child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", "sleep 1; cat > /dev/null"], stdin := .piped }
  let (stdin, child) ← child.takeStdin
  stdin.write (ByteArray.mk (Array.replicate 65536 120))
  stdin.flush
  stdin.putStr "x"
  -- `stdin`'s last use: its finalizer closes it here, before the resolution
  p.resolve 1
  let v ← IO.wait p.result?
  IO.println s!"main sees {v}"
  let _ ← IO.wait b
  let _ ← child.wait
  pure ()
