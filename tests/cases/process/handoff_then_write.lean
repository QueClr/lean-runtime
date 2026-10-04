/-! A FIFO opened twice for writing (leanrs's probe, lean-runtime fixes-1, AR-8): N bytes `X`
(from argv) go through the first handle, which is then dropped while the FIFO is full, and `Y`
through the second. The reader (a child) opens the FIFO at once but reads only after a second,
so the first handle's `fclose` blocks until then: natively `X`'s last bytes always come before
`Y`. In a translator whose drop hands the last bytes to a writer thread, the dropping context
waits for that writer before it takes the second stream's lock. The `.pipe` waits for the
child's marker, then prints the size and where `Y` is. -/
def main (args : List String) : IO Unit := do
  let f := args[0]!
  let n := args[1]!.toNat!
  let _ ← IO.Process.run { cmd := "mkfifo", args := #[f] }
  -- a program with tasks
  let _t ← IO.asTask (pure ())
  let _child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", s!"exec 3<{f}; sleep 1; cat <&3 > out; echo done > marker"] }
  let h1 ← IO.FS.Handle.mk f .write
  let h2 ← IO.FS.Handle.mk f .write
  h1.putStr ("".pushn 'X' n)
  -- `h1`'s last use: its finalizer closes it here, waiting for the reader
  h2.putStr "Y"
  h2.flush
  IO.println "written"
