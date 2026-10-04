/-! V11 row group: `IO.setStdout`, `IO.setStderr` and `IO.setStdin` (the thread's current standard
streams), with buffer streams (`IO.FS.Stream.ofBuffer`), restoring the previous stream, the previous
stream used directly, `IO.FS.withIsolatedStreams`, and reads of a replaced standard input. -/

def bufText (r : IO.Ref IO.FS.Stream.Buffer) : IO String := do
  return String.fromUTF8! (← r.get).data

def boom (n : Nat) : Nat := if n > 100 then n else panic! "boom"

def main : IO Unit := do
  IO.println "start"
  let buf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let old ← IO.setStdout (IO.FS.Stream.ofBuffer buf)
  IO.println "captured one"
  IO.print "captured two"
  old.putStrLn "through the previous stream"
  let mine ← IO.setStdout old
  mine.putStrLn " (and through the returned buffer stream)"
  IO.println s!"buffer: {repr (← bufText buf)}"
  -- standard error
  let ebuf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let olderr ← IO.setStderr (IO.FS.Stream.ofBuffer ebuf)
  IO.eprintln "error captured"
  let _ ← IO.setStderr olderr
  IO.eprintln "error to the real stream"
  IO.println s!"stderr buffer: {repr (← bufText ebuf)}"
  -- standard input from a buffer
  let ibuf ← IO.mkRef ({ data := "first line\nsecond line\nrest".toUTF8 } : IO.FS.Stream.Buffer)
  let oldin ← IO.setStdin (IO.FS.Stream.ofBuffer ibuf)
  let stdin ← IO.getStdin
  let a ← stdin.getLine
  let b ← stdin.getLine
  let c ← stdin.getLine
  let d ← stdin.getLine
  let _ ← IO.setStdin oldin
  IO.println s!"stdin lines: {repr a} {repr b} {repr c} {repr d}"
  let real ← (← IO.getStdin).getLine
  IO.println s!"real stdin: {repr real}"
  -- nested redirection
  let b1 ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let b2 ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let s0 ← IO.setStdout (IO.FS.Stream.ofBuffer b1)
  IO.println "to b1"
  let s1 ← IO.setStdout (IO.FS.Stream.ofBuffer b2)
  IO.println "to b2"
  let _ ← IO.setStdout s1
  IO.println "to b1 again"
  let _ ← IO.setStdout s0
  IO.println s!"b1: {repr (← bufText b1)} b2: {repr (← bufText b2)}"
  -- withIsolatedStreams
  let (out, r) ← IO.FS.withIsolatedStreams (do
    IO.println "isolated out"
    IO.eprintln "isolated err"
    return 42 : IO Nat)
  IO.println s!"isolated: {repr out} {r}"
  let (out2, _) ← IO.FS.withIsolatedStreams (isolateStderr := false) (do
    IO.println "isolated out only"
    IO.eprintln "not isolated err" : IO Unit)
  IO.println s!"isolated stdout only: {repr out2}"
  -- the runtime's own standard-error text, panic messages and timeit's line, goes through the current
  -- standard error stream (chapter 05 O9; Lean's io_eprintln)
  let cell ← IO.mkRef #[1, 2]
  let (perr, v) ← IO.FS.withIsolatedStreams (do
    let xs ← cell.get
    IO.println s!"got {xs[5]!} and {boom 3}"
    return 7 : IO Nat)
  IO.println s!"isolated panics: {repr perr} {v}"
  let tbuf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let olderr2 ← IO.setStderr (IO.FS.Stream.ofBuffer tbuf)
  let t ← timeit "timed" (pure 3)
  IO.println s!"panic under setStderr: {boom 4}"
  let _ ← IO.setStderr olderr2
  let ttext ← bufText tbuf
  let tlines := ttext.splitOn "\n"
  IO.println s!"timeit captured: {tlines.length} {(tlines.headD "").startsWith "timed "} {(tlines.headD "").endsWith "ms"} {repr (tlines.getD 1 "")} {t}"
  -- back on the real standard error
  IO.println s!"after restore: {boom 5}"
  IO.println "end"
