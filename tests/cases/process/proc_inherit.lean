import Std.Internal.UV.System
/-! V11 row group: what a child process inherits and where its start fails (chapter 05 O8 items 4
and 5): the parent's ignored `SIGPIPE` (a child writing into a closed pipe gets `EPIPE` and exits,
where a default disposition kills it with status 141), and a working directory that exists but cannot
be entered (no search permission), which Lean's child reports as the directory's failure; and the
environment a child gets from a parent whose `environ` holds entries `setenv` cannot name (`=x`, `=`,
an entry with no `=`) and a duplicated name: the fixture starts itself again with such an `envp`
(through `python3`'s `ctypes` `execve`), and that run spawns `env` with `inheritEnv := false`, with an
`unsetenv`, and unchanged, and lists its own environment after each (Lean changes only the forked
child's copy). -/

-- flushed, so that a child that cannot start finds no unwritten parent bytes to copy (O8 item 5)
def report (label : String) (o : IO.Process.Output) : IO Unit := do
  IO.println s!"{label}: exit {o.exitCode} out {repr o.stdout} err {repr o.stderr}"
  (← IO.getStdout).flush

def mode (r w x : Bool) : IO.FileRight :=
  { user := { read := r, write := w, execution := x } }

/-- The `execve` that starts the fixture again with an `envp` no `setenv` sequence can build. -/
def reexec : String :=
  "import ctypes, sys\n" ++
  "libc = ctypes.CDLL(None)\n" ++
  "exe = sys.argv[1].encode()\n" ++
  "env = [b'=x', b'A=1', b'=a=b', b'B=2', b'=', b'D=4', b'B=dup', b'noequals', b'PATH=/usr/bin:/bin']\n" ++
  "argv = (ctypes.c_char_p * 3)(exe, b'envchild', None)\n" ++
  "envp = (ctypes.c_char_p * (len(env) + 1))(*env, None)\n" ++
  "libc.execve(exe, argv, envp)\n" ++
  "sys.exit(127)\n"

def envChild : IO Unit := do
  let showO (label : String) (o : IO.Process.Output) : IO Unit := IO.println s!"{label}: {repr o.stdout}"
  showO "noinherit" (← IO.Process.output { cmd := "/usr/bin/env", inheritEnv := false, env := #[("Z", some "1")] })
  IO.println s!"after noinherit: {(← Std.Internal.UV.System.osEnviron).toList}"
  showO "changed" (← IO.Process.output { cmd := "/usr/bin/env", env := #[("A", none), ("B", some "9")] })
  IO.println s!"after changed: {(← Std.Internal.UV.System.osEnviron).toList}"
  showO "plain" (← IO.Process.output { cmd := "/usr/bin/env" })

def main (args : List String) : IO Unit := do
  if args == ["envchild"] then
    envChild
    return
  -- `yes` writes into a pipe whose reader `head` has exited: with SIGPIPE ignored it gets EPIPE, exit 1
  report "sigpipe" (← IO.Process.output { cmd := "sh", args := #["-c", "(yes 2>/dev/null; echo \"yes exited $?\" >&2) | head -n 1"] })
  -- a directory without search permission: chdir fails with EACCES
  IO.FS.createDir "locked"
  IO.setAccessRights "locked" (mode true true false)
  report "cwd-no-search" (← IO.Process.output { cmd := "pwd", cwd := some "locked" })
  report "cwd-no-search-missing-prog" (← IO.Process.output { cmd := "leanrs-no-such-program-xyz", cwd := some "locked" })
  -- search permission alone is enough to enter it
  IO.FS.createDir "searchonly"
  IO.setAccessRights "searchonly" (mode false false true)
  report "cwd-search-only" (← IO.Process.output { cmd := "sh", args := #["-c", "basename \"$PWD\""], cwd := some "searchonly" })
  -- a regular file and a path through one
  IO.FS.writeFile "plain" "x"
  report "cwd-file" (← IO.Process.output { cmd := "pwd", cwd := some "plain" })
  report "cwd-through-file" (← IO.Process.output { cmd := "pwd", cwd := some "plain/sub" })
  report "cwd-empty" (← IO.Process.output { cmd := "pwd", cwd := some "" })
  IO.setAccessRights "locked" (mode true true true)
  IO.setAccessRights "searchonly" (mode true true true)
  IO.FS.removeDir "locked"
  IO.FS.removeDir "searchonly"
  IO.FS.removeFile "plain"
  -- a parent environment no `setenv` sequence builds
  let self := (← IO.appPath).toString
  report "malformed environ" (← IO.Process.output { cmd := "python3", args := #["-c", reexec, self] })
  IO.println "end"
