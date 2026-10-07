/-! `IO.mapTask (sync := true) f t` on a finished `t` applies `f` at once on
the calling thread (`lean_task_map_core`): no task, so no "`Task.get`
called from a `(sync := true)` task" panic when `f` waits. `main` drops a
pipe handle whose last byte the pipe cannot take yet: natively its `fclose`
blocks until the child reads, about 1 s; `t` finishes at 300 ms; then `main`
maps over `t` with `sync := true`, and `f` waits for `slow` (1.5 s).
Natively: "dep 5 7", then "main after mapTask". Before review HR-02
(fixes-14) a translator whose drop hands the bytes to a writer thread saw
`t` unfinished, and `depend`'s writers point let it finish, after which
the scheduler queued the `sync` dependent: the panic, and the lines
reversed. Now the drain's end waits for the writer (`sched::after_drain`),
and `depend` runs a `sync` dependent of a finished task at once. -/
def main : IO Unit := do
  let t ← BaseIO.asTask (do IO.sleep 300; pure (5 : Nat))
  let slow ← BaseIO.asTask (do IO.sleep 1500; pure (7 : Nat))
  let child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", "sleep 1; cat > /dev/null"], stdin := .piped }
  let (stdin, child) ← child.takeStdin
  stdin.write (ByteArray.mk (Array.replicate 65536 120))
  stdin.flush
  stdin.putStr "x"
  -- `stdin`'s last use: its finalizer closes it here, before the map
  let d ← IO.mapTask (sync := true) (fun v => do
      let w ← IO.wait slow
      IO.println s!"dep {v} {w}") t
  IO.println "main after mapTask"
  let _ ← IO.wait d
  let _ ← child.wait
  pure ()
