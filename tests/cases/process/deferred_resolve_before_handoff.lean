/-! One free drops an array `#[stdin, p]`: natively `lean_del_core` reaches
the promise first (the last element), resolves it with `none`, then closes
the child's stdin, whose last byte waits until the child reads its stdin.
The child first writes 70000 bytes to its stdout, which a reader task reads
once the promise is resolved. Natively: "reader got 70000", "child 0".
Before review RF14-07 (fixes-14) a translator whose drop hands the stream's
bytes to a writer thread and defers the promise's resolution to the drain's
end waited there for the writer before it resolved the promise: the reader
never read, the child never read its stdin, and the program hung. The
other order, `#[p, stdin]`, deadlocks natively (the `fclose` blocks first). -/
def main : IO Unit := do
  let child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", "head -c 70000 /dev/zero; cat > /dev/null"],
      stdin := .piped, stdout := .piped }
  let (stdin, child) ← child.takeStdin
  let p ← IO.Promise.new (α := Unit)
  let r := p.result?
  let reader ← IO.asTask do
    let _ ← IO.wait r
    let out ← child.stdout.readToEnd
    return out.length
  -- the reader runs and blocks on the promise
  IO.sleep 50
  stdin.write (ByteArray.mk (Array.replicate 65536 120))
  stdin.flush
  stdin.putStr "x"
  let cell ← IO.mkRef (some #[Sum.inl stdin, Sum.inr p] :
    Option (Array (Sum IO.FS.Handle (IO.Promise Unit))))
  cell.set none
  match ← IO.wait reader with
  | .ok n => IO.println s!"reader got {n}"
  | .error e => IO.println s!"reader error {e}"
  let st ← child.wait
  IO.println s!"child {st}"
