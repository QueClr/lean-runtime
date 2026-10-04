/-! How the child finds its program: glibc's `execvp` (`__execvpe`, 2.39). An empty name is
`ENOENT`; a name with a `/` is tried as it is; otherwise each entry of the child's `PATH` (after
the `env` changes; `/bin:/usr/bin` without one; an empty entry is the working directory, after
`cwd`), going on after `EACCES`, `ENOENT`, `ENOTDIR` and the like; a file the kernel refuses with
`ENOEXEC` runs under `/bin/sh`. Each failure is the forked child's message and status 255. Lean
passes C strings: the program name, the arguments, the `cwd` and the environment end at their
first NUL byte. -/
def mode (r w x : Bool) : IO.FileRight := { user := { read := r, write := w, execution := x } }

def report (label : String) (args : IO.Process.SpawnArgs) : IO Unit := do
  try
    let o ← IO.Process.output args
    IO.println s!"{label}: exit {o.exitCode} out {repr o.stdout} err {repr o.stderr}"
  catch e => IO.println s!"{label}: error {e}"
  (← IO.getStdout).flush

def main : IO Unit := do
  IO.FS.createDirAll "bin1"
  IO.FS.createDirAll "bin2"
  IO.FS.createDirAll "work/bin3"
  -- an executable script without `#!` in bin2, the same name not executable in bin1
  IO.FS.writeFile "bin1/tool" "echo from bin1\n"
  IO.setAccessRights "bin1/tool" (mode true true false)
  IO.FS.writeFile "bin2/tool" "echo \"from bin2 $# $*\"\n"
  IO.setAccessRights "bin2/tool" (mode true true true)
  IO.FS.writeFile "work/bin3/other" "echo from bin3\n"
  IO.setAccessRights "work/bin3/other" (mode true true true)
  IO.FS.writeFile "plain" "x"
  let top := (← IO.currentDir).toString
  report "empty name" { cmd := "" }
  report "eacces then found" { cmd := "tool", args := #["a", "b"], env := #[("PATH", some s!"{top}/bin1:{top}/bin2")] }
  report "eacces only" { cmd := "tool", env := #[("PATH", some s!"{top}/bin1")] }
  report "enotdir entry" { cmd := "tool", env := #[("PATH", some s!"{top}/plain:{top}/bin2")] }
  report "empty entry" { cmd := "tool", env := #[("PATH", some ":/nonexistent")], cwd := some "bin2" }
  report "relative entry after cwd" { cmd := "other", env := #[("PATH", some "bin3")], cwd := some "work" }
  report "empty PATH" { cmd := "tool", env := #[("PATH", some "")], cwd := some "bin2" }
  report "no PATH" { cmd := "sh", args := #["-c", "echo default path"], env := #[("PATH", none)] }
  report "slash relative after cwd" { cmd := "./bin3/other", cwd := some "work" }
  report "directory" { cmd := "/tmp" }
  report "not executable with slash" { cmd := "bin1/tool" }
  report "long name" { cmd := String.ofList (List.replicate 300 'n') }
  report "nul in cmd" { cmd := "sh\x00junk", args := #["-c", "echo nul cmd"] }
  report "nul in cwd" { cmd := "sh", args := #["-c", "basename \"$PWD\""], cwd := some "work\x00junk" }
  report "nul in bad cwd" { cmd := "pwd", cwd := some "missing\x00junk" }
  report "nul in env name" { cmd := "sh", args := #["-c", "echo \"[${AB-unset}][${A-unset}]\""], env := #[("A\x00B", some "v")] }
