/-! A task blocked reading a pipe while `main` writes to it: `cat` copies its
standard input to its standard output; a task reads `cat`'s output to its end
while `main` writes more than both pipes hold to `cat`'s input. Natively the
task's thread reads while `main`'s writes block; on one thread `main`'s blocked
write must let the task run (sched-io). The line count and length come from
argv. -/

def writeAll (stdin : IO.FS.Handle) (n len : Nat) : IO Unit := do
  let line := String.ofList (List.replicate len 'x') ++ "\n"
  for _ in [0:n] do
    stdin.putStr line
  stdin.flush

def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let len := args[1]!.toNat!
  let child ← IO.Process.spawn { cmd := "cat", stdin := .piped, stdout := .piped }
  let (stdin, child) ← child.takeStdin
  let reader ← IO.asTask do
    let s ← child.stdout.readToEnd
    return (s.length, (s.splitOn "\n").length - 1)
  writeAll stdin n len
  IO.println "main: wrote everything"
  -- `stdin` is dropped here: `cat` sees the end of its input
  let r ← IO.wait reader
  match r with
  | .ok (bytes, lines) => IO.println s!"task: read {bytes} bytes in {lines} lines"
  | .error e => IO.println s!"task failed: {e}"
  let code ← child.wait
  IO.println s!"cat exited {code}"
