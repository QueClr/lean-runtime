/-! The io_uring rings libuv 1.48 makes at startup (`uv__iou_init`; finding A821): descriptor 4 is
the ring a kernel thread polls (64 entries, `IORING_SETUP_SQPOLL`) and 5 the control ring (256
entries). `/proc/self/fdinfo` shows each ring's masks (the entries less one, and twice the entries
less one for completions) and whether it has a polling thread. Each ring has an inode of its own,
while the epoll descriptor (3) and the eventfd share the kernel's anonymous inode. The epoll
descriptor watches the polling ring, and the polling thread is a task of the process, an io_uring
kernel thread (`PF_IO_WORKER`, 0x10, in the flags of its `stat`, field 9; its name, `iou-sqp-<pid>`,
is set by the thread itself when it first runs, so it is not read here). libuv maps each ring
twice (the submission and completion rings, and the submission entries), so `/proc/self/maps` has
four `anon_inode:[io_uring]` lines. The `.pipe` runs it again with `UV_USE_IO_URING=0`: no rings, and the later descriptors move down by
two. -/

/-- The value of `key` in a `/proc/self/fdinfo` text, spaces and tabs removed. -/
def field (info key : String) : Option String :=
  (info.splitOn "\n").findSome? fun l =>
    match l.splitOn ":" with
    | k :: rest => if k == key then some (String.join ((":".intercalate rest).toList.filter (fun c => c != ' ' && c != '\t') |>.map Char.toString)) else none
    | _ => none

def fdinfo (fd : Nat) : IO (Option String) := do
  try return some (← IO.FS.readFile s!"/proc/self/fdinfo/{fd}") catch _ => return none

def main (args : List String) : IO Unit := do
  let infos ← (List.range 8).mapM fun i => do return (i + 3, ← fdinfo (i + 3))
  let ino (fd : Nat) : Option String := (infos.lookup fd).join.bind (field · "ino")
  for (fd, info) in infos do
    match info with
    | none => IO.println s!"{fd}: closed"
    | some i =>
      match field i "SqMask", field i "CqMask", field i "SqThread" with
      | some sq, some cq, some th =>
        IO.println s!"{fd}: ring, SqMask {sq}, CqMask {cq}, polling thread {th != "-1"}, own inode {ino fd != ino 3}"
      | _, _, _ =>
        if (field i "eventfd-count").isSome then
          IO.println s!"{fd}: eventfd, shares the epoll descriptor's inode {ino fd == ino 3}"
        else
          IO.println s!"{fd}: not a ring"
  let watched := match (infos.lookup 3).join with
    | some i => (i.splitOn "\n").any fun l => l.startsWith "tfd:" && ((l.splitOn " ").filter (· != ""))[1]? == some "4"
    | none => false
  IO.println s!"the epoll descriptor watches 4: {watched}"
  let mut poller := false
  for e in ← System.FilePath.readDir "/proc/self/task" do
    let stat ← IO.FS.readFile (e.path / "stat")
    -- the fields after the name (which ends at the last `)`): state, ppid, pgrp, session, tty,
    -- tpgid, flags
    let fields := ((stat.splitOn ")").getLast!.splitOn " ").filter (· != "")
    if ((fields[6]?.bind String.toNat?).getD 0) &&& 0x10 != 0 then poller := true
  IO.println s!"an io_uring kernel thread among the tasks: {poller}"
  -- libuv maps each ring twice: the submission and completion rings, and the submission entries
  let maps ← IO.FS.readFile "/proc/self/maps"
  let mapped := ((maps.splitOn "\n").filter fun l => (l.splitOn "[io_uring]").length > 1).length
  IO.println s!"io_uring mappings in /proc/self/maps: {mapped}"
  IO.println s!"args: {args.length}"
