/-! V11 row group: `IO.Process.spawn` and the `Child` rows (`wait`, `tryWait`, `kill`, `takeStdin`,
`pid`): pipes in both directions, an inherited standard error, exit codes, a signal's exit code, a
missing executable, waiting twice, and the standard-output flush before a child that inherits standard
input. -/

def main : IO Unit := do
  -- cat with both pipes; closing standard input ends it
  let child ← IO.Process.spawn { cmd := "cat", stdin := .piped, stdout := .piped, stderr := .null }
  let (stdin, child) ← child.takeStdin
  stdin.putStr "through cat\nsecond line\n"
  stdin.flush
  let out ← child.stdout.readToEnd
  let code ← child.wait
  IO.println s!"cat: {code} {repr out}"
  -- standard error piped, nonzero exit
  let child ← IO.Process.spawn { cmd := "sh", args := #["-c", "echo e1 >&2; echo o1; exit 7"], stdout := .piped, stderr := .piped, stdin := .null }
  let err ← child.stderr.readToEnd
  let out ← child.stdout.readToEnd
  let code ← child.wait
  IO.println s!"sh: {code} {repr out} {repr err}"
  -- waiting twice
  try
    let again ← child.wait
    IO.println s!"second wait: {again}"
  catch e =>
    IO.println s!"second wait error: {e}"
  try
    let t ← child.tryWait
    IO.println s!"tryWait after wait: {t}"
  catch e =>
    IO.println s!"tryWait after wait error: {e}"
  -- kill a sleeping child
  let child ← IO.Process.spawn { cmd := "sleep", args := #["30"], stdin := .null, stdout := .null, stderr := .null }
  IO.println s!"pid positive: {decide (child.pid > 0)}"
  child.kill
  let code ← child.wait
  IO.println s!"killed: {code}"
  try
    child.kill
    IO.println "kill after wait: ok"
  catch e =>
    IO.println s!"kill after wait error: {e}"
  -- tryWait until the child exits
  let child ← IO.Process.spawn { cmd := "true", stdin := .null, stdout := .null, stderr := .null }
  let mut status := none
  while status.isNone do
    status ← child.tryWait
    if status.isNone then IO.sleep 5
  IO.println s!"tryWait: {status}"
  -- a missing executable: the child reports on its standard error, here piped
  let child ← IO.Process.spawn { cmd := "leanrs-no-such-program-xyz", stdout := .piped, stderr := .piped, stdin := .piped }
  let err ← child.stderr.readToEnd
  let out ← child.stdout.readToEnd
  let code ← child.wait
  IO.println s!"missing piped: {code} {repr out} {repr err}"
  -- and here inherited: the message goes to our standard error
  let child ← IO.Process.spawn { cmd := "leanrs-no-such-program-xyz", stdin := .null }
  let code ← child.wait
  IO.println s!"missing inherited: {code}"
  -- a child that inherits standard output and input: our buffered output is flushed first
  IO.print "before child; "
  let child ← IO.Process.spawn { cmd := "echo", args := #["child line"] }
  let code ← child.wait
  IO.println s!"after child {code}"
  -- inherited standard output, piped standard input: no flush first
  IO.print "unflushed; "
  let child ← IO.Process.spawn { cmd := "echo", args := #["second child"], stdin := .piped }
  let code ← child.wait
  IO.println s!"after second child {code}"
  -- setsid: the child leads a new session (field 6 of /proc/<pid>/stat is its pid)
  for setsid in [false, true] do
    let child ← IO.Process.spawn { cmd := "sh", args := #["-c", "set -- $(cat /proc/$$/stat); if [ \"$6\" = \"$$\" ]; then echo leader; else echo member; fi"], setsid, stdin := .null, stdout := .piped, stderr := .null }
    let out ← child.stdout.readToEnd
    let code ← child.wait
    IO.println s!"setsid {setsid}: {code} {repr out}"
