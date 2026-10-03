-- Writing to a closed standard output: what Lean's runtime reports and which
-- exit code it gives (run with stdout closed through <id>.pipe).

def main (args : List String) : IO Unit := do
  IO.println s!"hello {args.length}"
  IO.eprintln "after println"
