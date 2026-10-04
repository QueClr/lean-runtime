import Std.Internal.UV.System
/-! V11 row group: `Std.Internal.UV.System`'s queries (chapter 02 D21, chapter 05 D25), printed as values
that do not depend on the host: ids and paths compared with `IO`'s own queries, the environment the
case sets, the errors Lean's `uv/system.cpp` reports, and relations between the memory and CPU
figures. -/

open Std.Internal.UV.System

def tryP {α} [ToString α] (label : String) (x : IO α) : IO Unit := do
  try IO.println s!"{label}: {← x}" catch e => IO.println s!"{label}: error {e}"

def main : IO Unit := do
  tryP "title" (do return (← getProcessTitle).endsWith "uv_system")
  tryP "pid" (do return decide ((← osGetPid).toNat == (← IO.Process.getPID).toNat))
  tryP "ppid" (do return decide ((← osGetPpid) > 0))
  tryP "cwd" (do return decide ((← cwd) == (← IO.currentDir).toString))
  tryP "exePath" (do return decide ((← exePath) == (← IO.appPath).toString))
  tryP "chdir missing" (chdir "no-such-dir")
  tryP "chdir nul" (chdir "a\x00b")
  IO.FS.writeFile "plain.txt" "x"
  tryP "chdir file" (chdir "plain.txt")
  IO.FS.createDirAll "sub"
  tryP "chdir sub" (do chdir "sub"; return (← cwd).endsWith "/sub")
  tryP "chdir back" (do chdir ".."; return decide ((← cwd) == (← IO.currentDir).toString))
  IO.FS.removeFile "plain.txt"
  IO.FS.removeDir "sub"
  tryP "homedir" (do return decide (some (← osHomedir) == (← IO.getEnv "HOME")))
  tryP "tmpdir" osTmpdir
  tryP "passwd" (do
    let p ← osGetPasswd
    return s!"{!p.username.isEmpty} {p.uid.isSome} {p.gid.isSome} {p.shell.isSome} {p.homedir.isSome}")
  tryP "environ" (do return s!"{(← osEnviron).toList.filter (·.1.startsWith "IOFIX")}")
  tryP "getenv" (do return s!"{← osGetenv "IOFIX_A"} {← osGetenv "IOFIX_NONE"} {← osGetenv "a\x00b"} {← osGetenv ""}")
  tryP "setenv" (do osSetenv "IOFIX_SET" "by uv"; return s!"{← osGetenv "IOFIX_SET"} {← IO.getEnv "IOFIX_SET"}")
  tryP "setenv overwrite" (do osSetenv "IOFIX_A" "again"; return s!"{← osGetenv "IOFIX_A"}")
  tryP "setenv =" (osSetenv "A=B" "x")
  tryP "setenv empty" (osSetenv "" "x")
  tryP "setenv nul" (osSetenv "A\x00" "x")
  tryP "setenv nul value" (osSetenv "A" "x\x00y")
  tryP "unsetenv" (do osUnsetenv "IOFIX_SET"; return s!"{← osGetenv "IOFIX_SET"}")
  tryP "unsetenv =" (osUnsetenv "A=B")
  tryP "unsetenv nul" (osUnsetenv "A\x00")
  tryP "unsetenv missing" (osUnsetenv "IOFIX_NEVER_SET")
  tryP "environ after" (do return s!"{(← osEnviron).toList.filter (·.1.startsWith "IOFIX")}")
  tryP "hostname" (do
    let h ← osGetHostname
    return decide (h == ((← IO.FS.readFile "/proc/sys/kernel/hostname").dropEnd 1).toString))
  tryP "uname" (do
    let u ← osUname
    let rel ← IO.FS.readFile "/proc/sys/kernel/osrelease"
    return s!"{u.sysname} {decide (u.release == (rel.dropEnd 1).toString)} {!u.version.isEmpty} {!u.machine.isEmpty}")
  tryP "uptime" (do return decide ((← uptime) > 0))
  tryP "hrtime" (do let a ← hrtime; IO.sleep 2; let b ← hrtime; return decide (a > 0 && b ≥ a + 2000000))
  tryP "rusage" (do
    let r ← getrusage
    return s!"{decide (r.maxRSS > 0)} {r.ixRSS} {r.idRSS} {r.isRSS} {r.nSwap} {r.msgSent} {r.msgRecv} {r.signals}")
  tryP "memory" (do
    let free ← freeMemory
    let total ← totalMemory
    let avail ← availableMemory
    let c ← constrainedMemory
    return s!"{decide (free > 0)} {decide (total > free)} {decide (avail > 0)} {decide (c == 0 || avail ≤ c || avail ≤ free)}")
  -- the scheduling priority (nice value): pids and priorities are C ints (their low 32 bits)
  tryP "priority self" (do return decide ((← osGetPriority 0) == (← osGetPriority (← osGetPid))))
  tryP "priority 2^32" (do return decide ((← osGetPriority 4294967296) == (← osGetPriority 0)))
  tryP "priority missing" (osGetPriority 2147483647)
  tryP "priority negative pid" (osGetPriority 4294967295)
  tryP "set priority same" (do osSetPriority 0 (← osGetPriority 0))
  tryP "set priority 20" (osSetPriority 0 20)
  tryP "set priority -21" (osSetPriority 0 (-21))
  tryP "set priority missing" (osSetPriority 2147483647 1)
  tryP "set priority negative pid" (osSetPriority 4294967295 1)
  tryP "set priority 2^32+19" (do osSetPriority 0 4294967315; return (← osGetPriority 0))
  tryP "cpus" (do
    let c ← cpuInfo
    let stat ← IO.FS.readFile "/proc/stat"
    let n := ((stat.splitOn "\n").drop 1).takeWhile (·.startsWith "cpu") |>.length
    return s!"{decide (c.size == n)} {c.all (!·.model.isEmpty)} {c.all (·.times.user % 10 == 0)}")
