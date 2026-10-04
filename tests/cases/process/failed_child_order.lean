/-! When a child cannot start, Lean's forked child writes the parent's pending standard-output bytes
and its message about a millisecond after the spawn, so the parent's next standard-error line
comes first (leanrs review LRIO2-F2: 49 of 50 native runs). The streams are merged; the pending
`pending ` is written twice, by the child's copy and by the parent's exit. -/
def main (args : List String) : IO Unit := do
  IO.print "pending "
  let c ← IO.Process.spawn { cmd := args.headD "no-such-program-xyz", stdin := .null }
  IO.eprintln "parent after spawn"
  let code ← c.wait
  IO.eprintln s!"waited {code}"
