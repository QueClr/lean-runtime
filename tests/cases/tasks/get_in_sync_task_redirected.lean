-- The contrast of `result_bang_dropped_redirected`: the panic of `Task.get`
-- in a `sync := true` task is a `lean_panic` without `force_stderr`, so its
-- line goes to Lean's current stderr (`io_eprintln`), here a buffer set by
-- `IO.setStderr`. `main` resolves the promise, so the dependent runs on
-- `main`'s thread, whose stderr is the buffer; the buffer is then printed.

def main (args : List String) : IO Unit := do
  let ms := args.head!.toNat!
  let buf ← IO.mkRef {}
  let old ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
  let p ← IO.Promise.new (α := Unit)
  let tb ← IO.asTask (do IO.sleep ms.toUInt32; return 5)
  let d ← IO.mapTask (sync := true) (fun _ => do
    let v ← IO.wait tb
    IO.println s!"dependent got {repr v.toOption}") p.result?
  p.resolve ()
  let _ ← IO.wait d
  let _ ← IO.setStderr old
  let b ← buf.get
  IO.println s!"captured: {repr (String.fromUTF8! b.data)}"
  IO.eprintln "main done"
