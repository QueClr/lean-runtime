/-! Chapter 06 A4 fixture A755 (kind `io`): input written to a child that cannot start (a missing
program, a `cwd` that cannot be entered). Lean's forked child holds the read end of its standard-input
pipe until it exits, so `IO.Process.output` with `some input` and a write to `spawn`'s piped standard
input succeed up to the pipe's capacity (exit 255 with the child's message) and fail with `EPIPE`
beyond it, where the runtime's built child (chapter 05 O8 item 5) had dropped the read end, so every
non-empty write failed with `EPIPE`. Found by the tester (A748 there) and the breaker of sysio
(eac34312). -/
def output (label : String) (args : IO.Process.SpawnArgs) (input : String) : IO Unit := do
  try
    let o ← IO.Process.output args (some input)
    IO.println s!"{label}: exit {o.exitCode} out {repr o.stdout} err {repr o.stderr}"
  catch e => IO.println s!"{label}: error {e}"
  (← IO.getStdout).flush

def main (args : List String) : IO Unit := do
  let input := args.headD "data"
  output "output" { cmd := "no-such-program-xyz" } input
  output "output-empty" { cmd := "no-such-program-xyz" } ""
  output "output-bad-cwd" { cmd := "cat", cwd := some "no-such-dir" } input
  output "output-big" { cmd := "no-such-program-xyz" } (String.ofList (List.replicate 70000 'x'))
  let c ← IO.Process.spawn { cmd := "no-such-program-xyz", stdin := .piped, stdout := .null, stderr := .null }
  let (stdin, c) ← c.takeStdin
  try
    stdin.putStr input
    stdin.flush
    IO.println "spawn stdin write: ok"
  catch e => IO.println s!"spawn stdin write: error {e}"
  IO.println s!"code {← c.wait}"
  let c ← IO.Process.spawn { cmd := "no-such-program-xyz", stdin := .piped, stdout := .null, stderr := .null }
  try
    c.stdin.putStr (String.ofList (List.replicate 70000 'y'))
    c.stdin.flush
    IO.println "spawn big write: ok"
  catch e => IO.println s!"spawn big write: error {e}"
  IO.println s!"code {← c.wait}"
