-- Two workers (`LEAN_NUM_THREADS=2`). Pure task `p` is taken by a worker:
-- it prints a trace line on its thread's stderr, then computes. IO task `x`
-- then runs on the other worker, sets its stderr to a buffer and does not
-- restore it. Natively `p` runs on its own worker's thread, so its trace
-- goes to the process's stderr, and the buffer holds only `x`'s line.
-- Review RF3-01 of lean-runtime's fixes-3: a started pure task took its
-- emulated worker's id only when it ran, so it ran with `x`'s leftover
-- streams, and its trace went into `x`'s buffer.

def slow (n : Nat) : Nat := Id.run do
  let mut s := 0
  for i in [0:n] do s := (s + i * i) % 1000003
  return s

def main (args : List String) : IO Unit := do
  let k := args.head!.toNat!
  let buf ← IO.mkRef ({} : IO.FS.Stream.Buffer)
  let p := Task.spawn fun _ => dbgTrace "p's trace" fun _ => slow k
  let _ ← IO.hasFinished p
  IO.sleep 20
  let x ← IO.asTask (do
    let _ ← IO.setStderr (IO.FS.Stream.ofBuffer buf)
    IO.eprintln "x")
  let _ ← IO.wait x
  IO.println s!"p = {p.get}"
  IO.println s!"x's buffer: {repr (String.fromUTF8! (← buf.get).data)}"
