/-! Chapter 06 A4 fixture A753 (kind `io`): structure updates of the runtime mirror `IO.Process.Output`
through a function taking it owned or borrowed, a closure, a bind's continuation, and a constructor
reusing two fields, and of `IO.FS.Metadata`. MiniRust's check refused the update
of a mirror struct ("CtorFields.update: struct update leanrs_rt::io::Output: not a struct of the crate
with named fields", exit 4). Found by the breaker of sysio (eac34312, U/Out2.lean, T/ProcInput.lean). -/
@[noinline] def viaDef (o : IO.Process.Output) : IO.Process.Output := { o with stdout := "def" }
@[noinline] def viaBorrow (o : @& IO.Process.Output) : IO.Process.Output := { o with exitCode := 9 }
@[noinline] def twoKept (o : IO.Process.Output) : IO.Process.Output := ⟨o.exitCode + 1, o.stdout, "two"⟩
@[noinline] def touch (m : IO.FS.Metadata) : IO.FS.Metadata := { m with byteSize := m.byteSize + 1 }

def main : IO Unit := do
  let o ← IO.Process.output { cmd := "echo", args := #["hi"] }
  let a := viaDef o
  let b := viaBorrow o
  let c := (some o).map fun o => { o with stderr := "closure" }
  let d ← (IO.Process.output { cmd := "echo", args := #["bind"] }) >>= fun o => pure { o with stdout := toString o.stdout.length }
  let e := twoKept o
  IO.println s!"{repr a.stdout} {b.exitCode} {repr (c.map (·.stderr))} {repr d.stdout} {e.exitCode} {repr e.stdout} {repr e.stderr} {o.exitCode}"
  IO.FS.writeFile "f.txt" "abc"
  let m ← System.FilePath.metadata "f.txt"
  IO.println s!"{(touch m).byteSize} {m.byteSize}"
  IO.FS.removeFile "f.txt"
