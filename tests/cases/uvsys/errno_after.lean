import Std.Internal.UV.System
/-! The modelled `errno` after io-2's rows (review RIO2-02), observed as Lean observes it: a
handle opened for reading has its sticky error flag set by a failed write (`EBADF`), so a later
`getLine` reports whatever `errno` holds then. Successes that clear `errno` natively: the
temporary file and directory (libuv's `uv__fs_work` sets it to 0, glibc's `__gen_tempname`
restores it), `osGetPriority` (libuv clears it before `getpriority`), `osGetPasswd`, `osGetGroup`
and `osHomedir` without `HOME` (glibc's `getpwuid_r`/`getgrgid_r` leave it at their result, 0).
Errors libuv makes itself leave it alone (`ENOBUFS`, `E2BIG`, the priority range check, the empty
`TMPDIR`); a failing system call sets it (`ESRCH`; `getcwd`'s, below). Based on the reviewer's native repro (review-io2/repro/errno).
That is native's outcome (`native`): since LB-41 the correct `getLine` reports only its own error,
so each probe's `getLine` reads its line. -/
open Std.Internal.UV.System

def probe (label : String) (act : IO Unit) : IO Unit := do
  IO.FS.writeFile "e.txt" "line1\n"
  let h ← IO.FS.Handle.mk "e.txt" .read
  try h.putStr "x"; h.flush catch _ => pure ()
  try act catch e => IO.println s!"{label} act: {e}"
  try
    let l ← h.getLine
    IO.println s!"{label}: ok {repr l}"
  catch e => IO.println s!"{label}: {e}"

/-- Enter a directory chain whose path is `len` bytes long; return the top to remove. -/
def deepDir (len : Nat) : IO System.FilePath := do
  let top := (← IO.currentDir) / "deep"
  IO.FS.createDir top
  IO.Process.setCurrentDir top
  let mut cur := top.toString.utf8ByteSize
  while len - cur > 251 do
    let d := String.ofList (List.replicate 200 'd')
    IO.FS.createDir d
    IO.Process.setCurrentDir d
    cur := cur + 201
  let d := String.ofList (List.replicate (len - cur - 1) 'd')
  IO.FS.createDir d
  IO.Process.setCurrentDir d
  return top

def main (args : List String) : IO Unit := do
  match args with
  | ["home"] => probe "osHomedir without HOME" (do discard <| osHomedir)
  | ["with-home"] => probe "osHomedir with HOME" (do discard <| osHomedir)
  | ["tmp"] =>
    probe "createTempFile tmpdir error" (do discard <| IO.FS.createTempFile)
    probe "osTmpdir error" (do discard <| osTmpdir)
  | _ =>
    let ps ← IO.mkRef (#[] : Array System.FilePath)
    probe "baseline" (pure ())
    probe "createTempFile" (do let (_, p) ← IO.FS.createTempFile; ps.modify (·.push p))
    probe "createTempDir" (do let p ← IO.FS.createTempDir; ps.modify (·.push p))
    probe "osGetPriority" (do discard <| osGetPriority 0)
    probe "osGetPasswd" (do discard <| osGetPasswd)
    probe "osGetGroup" (do discard <| osGetGroup 0)
    probe "osTmpdir" (do discard <| osTmpdir)
    probe "osSetPriority 20" (osSetPriority 0 20)
    probe "random big" (do discard <| random 0x80000000)
    probe "osGetPriority negative pid" (do discard <| osGetPriority 4294967295)
    probe "spawn and wait" (do let c ← IO.Process.spawn { cmd := "true" }; discard <| c.wait)
    probe "output" (do discard <| IO.Process.output { cmd := "true" })
    probe "cpuInfo" (do discard <| cpuInfo)
    for p in ← ps.get do
      if ← p.isDir then IO.FS.removeDir p else IO.FS.removeFile p
    -- a path of 4096 bytes or more is ENAMETOOLONG to the kernel's getcwd, and glibc's generic
    -- getcwd then walks the tree, clearing errno before each readdir: libuv's second getcwd
    -- succeeds for 4096 bytes (errno 0, libuv's ENOBUFS) and fails with ERANGE beyond
    for len in [4096, 4297] do
      let home ← IO.currentDir
      let top ← deepDir len
      probe s!"cwd of {len} bytes" (do discard <| cwd)
      IO.Process.setCurrentDir home
      discard <| IO.Process.output { cmd := "rm", args := #["-rf", top.toString] }
