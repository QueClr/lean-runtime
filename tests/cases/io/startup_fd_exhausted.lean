/-! Startup with too few descriptors for libuv's event loop (LB-30, LB-31). Lean starts libuv's
default loop before any module code, and libuv opens, at the lowest free numbers: an epoll
descriptor, two io_uring rings (where the kernel gives them), the signal lock pipe, the loop's
signal pipe and an eventfd (`io::startup`). The `.pipe` runs the program under `ulimit -n` 6, 9
and 10, then 5 and 7 with `UV_USE_IO_URING=0` (no rings), each followed by `echo "exit $?"`.
Natively: 6 and 5 leave no room for the signal lock pipe, and libuv calls `abort()` (134,
LB-31); 9, 10 and 7 leave room for it but not for the loop, `uv_default_loop()` returns NULL and
Lean uses it unchecked (SIGSEGV, 139, LB-30). Correct (both translators): an orderly internal
panic, `INTERNAL PANIC: Failed to initialize event loop: too many open files`, status 1, as
Lean's own `check_uv` ends a failed step of the same initialization. `main` (the descriptors open
in it, as `startup_fd_limit` prints them) is never reached. -/
def main : IO Unit := do
  let entries ← System.FilePath.readDir "/proc/self/fd"
  let mut fds : Array Nat := #[]
  for e in entries do
    if let some n := e.fileName.toNat? then fds := fds.push n
  IO.println s!"main: open {fds.qsort (· < ·)}"
