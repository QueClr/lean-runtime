/-! A `null` stream of a child: Lean's forked child opens `/dev/null` and `dup2`s it onto 0, 1 or 2
without closing the descriptor it opened (process.cpp, `spawn`), which is not close-on-exec, so
the program it runs has one more descriptor on `/dev/null` per `null` stream (LB-15). The case
counts the child's descriptors above 2 that name `/dev/null` (their numbers depend on the
descriptors the parent holds). The shared runtime opens `/dev/null` close-on-exec in the parent
and `posix_spawn` `dup2`s it, so none leaks (the expected outcome). -/
def script : String :=
  "n=0; for f in /proc/$$/fd/*; do i=${f##*/}; if [ \"$i\" -gt 2 ] && [ \"$(readlink $f)\" = /dev/null ]; then n=$((n+1)); fi; done; echo $n >&2"

def main : IO Unit := do
  let c ← IO.Process.spawn { cmd := "sh", args := #["-c", script], stdin := .null, stdout := .null, stderr := .piped }
  let r ← c.stderr.readToEnd
  let _ ← c.wait
  IO.println s!"stdin and stdout null: {r.trimAscii}"
  let c ← IO.Process.spawn { cmd := "sh", args := #["-c", script], stdin := .piped, stdout := .null, stderr := .piped }
  let r ← c.stderr.readToEnd
  let _ ← c.wait
  IO.println s!"stdout null: {r.trimAscii}"
  let c ← IO.Process.spawn { cmd := "sh", args := #["-c", script], stdin := .piped, stdout := .piped, stderr := .piped }
  let r ← c.stderr.readToEnd
  let _ ← c.wait
  IO.println s!"none null: {r.trimAscii}"
