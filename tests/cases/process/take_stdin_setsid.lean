/-! `takeStdin` returns a new child object built without the `setsid` flag
(process.cpp, `lean_io_process_child_take_stdin`: its scalar area holds the pid only, and the
flag reads the zeroed padding). So natively `kill` on that child signals the shell alone, with
`kill`, not its process group with `killpg`, and the shell's background grandchild survives
(LB-14: both translators keep the flag); the case kills the grandchild itself afterwards. -/
def check (take : Bool) : IO Unit := do
  let c ← IO.Process.spawn { cmd := "sh", args := #["-c", "sleep 30 & echo $!; cat >/dev/null; wait"], stdin := .piped, stdout := .piped, setsid := true }
  let line ← c.stdout.getLine
  let gc := line.trimAscii.toString
  let code ← if take then do
      let (_, c) ← c.takeStdin
      c.kill
      c.wait
    else do
      c.kill
      c.wait
  -- A killed process may take a moment to exit (and to be reaped) on a loaded host: poll for up to 3 s.
  let chk ← IO.Process.output { cmd := "sh", args := #["-c", s!"i=0; while kill -0 {gc} 2>/dev/null && [ $i -lt 60 ]; do sleep 0.05; i=$((i+1)); done; kill -0 {gc} 2>/dev/null && echo alive || echo gone"] }
  IO.println s!"takeStdin {take}: killed {code}, grandchild {chk.stdout.trimAscii}"
  discard <| IO.Process.output { cmd := "sh", args := #["-c", s!"kill -9 {gc} 2>/dev/null; true"] }

def main : IO Unit := do
  check false
  check true
