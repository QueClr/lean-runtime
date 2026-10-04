/-! `IO.Process.Child.wait` in one task while another task runs: the waiter
blocks in `waitpid` for a child that sleeps, while a ticker prints every
50 ms. Natively the waiter's thread blocks and the ticker goes on; on one
thread the wait must let the ticker run (sched-io). The child's sleep and
the tick count come from argv. -/

def main (args : List String) : IO Unit := do
  let ticks := args[1]!.toNat!
  let child ← IO.Process.spawn { cmd := "sh", args := #["-c", "sleep \"$1\"; exit 7", "sh", args[0]!] }
  let waiter ← IO.asTask (prio := .dedicated) do
    let c ← child.wait
    IO.println s!"waiter: child exited {c}"
    return c
  let ticker ← IO.asTask do
    for i in [0:ticks] do
      IO.sleep 50
      IO.println s!"ticker: {i}"
  let _ ← IO.wait ticker
  IO.println "main: ticker done"
  let r ← IO.wait waiter
  IO.println s!"main: waiter returned {repr r.toOption}"
