/-! `output_drain_exit`, ended by an internal panic (`Array.replicate` of 2^64 elements, the size from
argv: `INTERNAL PANIC: out of memory`) instead of `main`'s return (AR-6, leanrs's review of
fixes-1; LB-29). Natively `lean_internal_panic`'s `exit(1)` waits, in its flush of every `FILE`,
for the lock that the standard-output reader of `output` holds in `fread` until the child's end
of file: the child's late write finds a reader (status 0). Correct (LB-29): the process ends at
once, and the late write fails with `EPIPE` (status 1). The `.pipe` waits for the file `marker`,
then prints it. -/
def main (args : List String) : IO Unit := do
  try
    let o ← IO.Process.output { cmd := "sh", args := #["-c",
      "printf '\\377' >&2; exec 2>&-; sleep 1; echo late; echo \"child: write status $?\" > marker"] }
    IO.println s!"output: exit {o.exitCode}"
  catch e => IO.println s!"output failed: {e}"
  IO.println "main ends"
  IO.println s!"{(Array.replicate args[0]!.toNat! "s").size}"
