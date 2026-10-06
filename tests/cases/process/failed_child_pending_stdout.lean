/-! A child that cannot start, with output pending in the parent's standard-output buffer (LB-42
of `docs/lean-bugs.md`; io bug hunt HIO-03). Natively the forked child writes its copy of the
pending bytes before its message (`std::cerr` is tied to `std::cout`, process.cpp 511), so
`output` returns the parent's pending bytes as the child's standard output, and the parent writes
them again at its exit (`native`). The correct outcome: the child's standard output is empty.
The program comes from argv (a missing one by default). -/
def main (args : List String) : IO Unit := do
  IO.print "pending "
  let out ← IO.Process.output { cmd := args.headD "no-such-program-xyz" }
  IO.println s!"\nexit {out.exitCode} stdout {repr out.stdout} stderr {repr out.stderr}"
