-- The descriptors native Lean has open before `main` (libuv's loop: an
-- epoll descriptor, two io_uring rings, two pipes, an eventfd; finding A821),
-- and how many files a program can open before `EMFILE`. .pipe runs it under
-- `ulimit -n 64`, then again with stdin closed (the epoll descriptor takes
-- number 0), then with `UV_USE_IO_URING=0`, which makes libuv skip both
-- rings. Then the boundary, where no descriptor is left once libuv's are
-- open (`ulimit -n 11`; 10 with stdin closed; 9 with `UV_USE_IO_URING=0`):
-- Init's module initializer `IO.stdGenRef` (Init/Data/Random.lean) cannot
-- open `/dev/urandom` for `IO.getRandomBytes`, and the program ends with
-- that uncaught error, status 1, before `main`. The path and the cap come
-- from argv (from lean2rr's tests/runtime/RtFdLimit.lean).

def openUntilFull (path : String) : Nat → Array IO.FS.Handle → IO (Array IO.FS.Handle × String)
  | 0, acc => return (acc, "limit not reached")
  | n + 1, acc => do
    match ← (IO.FS.Handle.mk path .read).toBaseIO with
    | .ok h => openUntilFull path n (acc.push h)
    | .error e => return (acc, toString e)

def main (args : List String) : IO Unit := do
  let entries ← System.FilePath.readDir "/proc/self/fd"
  let mut fds : Array Nat := #[]
  for e in entries do
    if let some n := e.fileName.toNat? then fds := fds.push n
  -- The listing's own directory descriptor is the first free number.
  IO.println s!"open at startup: {fds.qsort (· < ·)}"
  let (hs, err) ← openUntilFull args[0]! args[1]!.toNat! #[]
  IO.println s!"opened {hs.size} more, then: {err}"
