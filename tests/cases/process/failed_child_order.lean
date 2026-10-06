/-! When a child cannot start, Lean's forked child writes its message about a millisecond after
the spawn, so the parent's next standard-error line comes first (leanrs review LRIO2-F2: 49 of
50 native runs). The streams are merged. Natively the child also writes its copy of the pending
`pending ` before its message, and the parent writes it again at its exit (LB-42 of
`docs/lean-bugs.md`); the correct outcome has it once, from the parent. -/
def main (args : List String) : IO Unit := do
  IO.print "pending "
  let c ← IO.Process.spawn { cmd := args.headD "no-such-program-xyz", stdin := .null }
  IO.eprintln "parent after spawn"
  let code ← c.wait
  IO.eprintln s!"waited {code}"
