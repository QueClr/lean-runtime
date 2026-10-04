/-! `IO.getRandomBytes` opens `/dev/urandom` before it allocates its array
(`lean_io_get_random_bytes`, io.cpp 866-878; AR-1, from leanrs's adopt-io review). With no
descriptor left (`ulimit -n 64`, then files opened until `EMFILE`), a request of 2^50 bytes,
too big to allocate, fails with the open's catchable error naming `/dev/urandom`, not with
`INTERNAL PANIC: out of memory`; so does a small one, and a request of 0 bytes opens nothing.
The sizes come from argv. -/
partial def exhaust (acc : Array IO.FS.Handle) : IO (Array IO.FS.Handle) := do
  match ← (IO.FS.Handle.mk "/dev/null" .read).toBaseIO with
  | .ok h => exhaust (acc.push h)
  | .error _ => pure acc

def attempt (n : USize) : IO String := do
  match ← (IO.getRandomBytes n).toBaseIO with
  | .ok b => pure s!"{b.size} bytes"
  | .error e => pure s!"error: {e}"

def main (args : List String) : IO Unit := do
  let big := args[0]!.toNat!.toUSize
  let small := args[1]!.toNat!.toUSize
  let hs ← exhaust #[]
  IO.println s!"getRandomBytes {big}: {← attempt big}"
  IO.println s!"getRandomBytes {small}: {← attempt small}"
  IO.println s!"getRandomBytes 0: {← attempt 0}"
  -- keeps the handles open until here (their only use, or the compiler frees them early)
  IO.println s!"handles kept open: {decide (hs.size > 0)}"
