/-! V11 row group: `IO.Process.output` and `IO.Process.run` (chapter 05 section 3.5, process rows):
pipes to `echo`, `cat` and `sh`, standard input given to the child, nonzero exit codes, standard error,
a missing executable, the child's working directory and environment. -/

def report (label : String) (o : IO.Process.Output) : IO Unit :=
  IO.println s!"{label}: exit {o.exitCode} out {repr o.stdout} err {repr o.stderr}"

def main : IO Unit := do
  report "echo" (← IO.Process.output { cmd := "echo", args := #["hello", "pipe world"] })
  report "cat" (← IO.Process.output { cmd := "cat" } (some "line one\nline two\n"))
  report "cat-empty" (← IO.Process.output { cmd := "cat" })
  report "sh" (← IO.Process.output { cmd := "sh", args := #["-c", "echo to-out; echo to-err >&2; exit 3"] })
  report "missing" (← IO.Process.output { cmd := "leanrs-no-such-program-xyz", args := #["a"] })
  report "slash-missing" (← IO.Process.output { cmd := "./no/such/prog" })
  report "cwd" (← IO.Process.output { cmd := "sh", args := #["-c", "basename \"$PWD\""], cwd := some "sub" })
  report "bad-cwd" (← IO.Process.output { cmd := "pwd", cwd := some "no-such-dir" })
  report "env-set" (← IO.Process.output { cmd := "sh", args := #["-c", "echo \"[$IOFIX_A][$IOFIX_B][${IOFIX_PARENT-unset}]\""], env := #[("IOFIX_A", some "alpha"), ("IOFIX_B", some "beta"), ("IOFIX_A", some "again"), ("IOFIX_PARENT", none)] })
  report "env-inherit" (← IO.Process.output { cmd := "sh", args := #["-c", "echo \"[${IOFIX_PARENT-unset}]\""] })
  report "env-clear" (← IO.Process.output { cmd := "/usr/bin/env", inheritEnv := false, env := #[("ONLY", some "this one")] })
  report "env-order" (← IO.Process.output { cmd := "/usr/bin/env", env := #[("ZZ_NEW", some "1"), ("HOME", some "home2"), ("PATH", none), ("AA_NEW", some "2"), ("ZZ_NEW", none), ("ZZ_NEW", some "3")] })
  -- names `setenv` refuses (empty, holding `=`) change nothing in Lean's child
  report "env-bad-names" (← IO.Process.output { cmd := "sh", args := #["-c", "echo \"[${IOFIX_OK-unset}]\""], env := #[("", some "x"), ("A=B", some "y"), ("IOFIX_OK", some "ok"), ("=", none)] })
  -- Lean passes C strings: an argument, a value and the program name end at their first NUL byte
  report "nul-args" (← IO.Process.output { cmd := "echo\x00ignored", args := #["one\x00two", "three"] })
  report "nul-env" (← IO.Process.output { cmd := "sh", args := #["-c", "echo \"[$IOFIX_NUL]\""], env := #[("IOFIX_NUL", some "before\x00after")] })
  report "env-nopath" (← IO.Process.output { cmd := "env", env := #[("PATH", none), ("HOME", none)] })
  report "big" (← IO.Process.output { cmd := "sh", args := #["-c", "i=0; while [ $i -lt 3000 ]; do echo line-$i; echo err-$i >&2; i=$((i+1)); done"] })
  report "signal" (← IO.Process.output { cmd := "sh", args := #["-c", "kill -9 $$"] })
  let s ← IO.Process.run { cmd := "printf", args := #["%s|%s", "x", "y z"] }
  IO.println s!"run: {repr s}"
  try
    let _ ← IO.Process.run { cmd := "sh", args := #["-c", "echo partial; echo why >&2; exit 2"] }
    IO.println "run: no error"
  catch e =>
    IO.println s!"run error: {e}"
  try
    let _ ← IO.Process.run { cmd := "leanrs-no-such-program-xyz" }
    IO.println "run: no error"
  catch e =>
    IO.println s!"run missing: {e}"
  -- a script without `#!`: the kernel refuses it (`ENOEXEC`) and `execvp` runs it with `/bin/sh`
  report "script" (← IO.Process.output { cmd := "sub/noshebang", args := #["x", "y"] })
  report "script-path" (← IO.Process.output { cmd := "noshebang", args := #["z"], env := #[("PATH", some "/nonexistent:sub")] })
  try
    let o ← IO.Process.output { cmd := "printf", args := #["\\377\\376"] }
    IO.println s!"invalid utf8: {o.exitCode}"
  catch e =>
    IO.println s!"invalid utf8 error: {e}"
