-- `dbgTrace msg f` prints `msg` to the *current* standard error stream (the
-- one `IO.setStderr` installed, as `io_eprintln` uses) and then returns
-- `f ()`. Inside `IO.withStderr` the message goes to the buffer; afterwards
-- to the process's stderr. The traced value depends on argv and, inside
-- `withStderr`, on a read of the buffer there, so the compiler cannot move
-- its computation out of the redirected block.

@[noinline] def traced (n : Nat) : Nat := dbgTrace s!"trace {n}" fun _ => n + 1

def main (args : List String) : IO Unit := do
  let n := args.length
  IO.eprintln "before"
  let buf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let v ← IO.withStderr (IO.FS.Stream.ofBuffer buf) do
    let b ← buf.get
    let v := traced (n + b.data.size)
    IO.println s!"value {v}"
    pure v
  IO.println s!"captured {repr (String.fromUTF8! (← buf.get).data)}"
  IO.eprintln s!"after {traced (v + 1)}"
