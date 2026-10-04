/-! `IO.Process.exit` while a task is blocked writing a large buffer to a child's stdin pipe (the
judge's `ExitWhileWriting.lean`, on LB-29's scope). Arguments: MODE SIZE OUTFILE; `slow`: the
child (python3) reads 64 KiB every 50 ms and, at the end of its input, writes the count it got to
OUTFILE; `never`: the child (`sleep 30`) never reads. C's `exit` flushes every stream with unwritten
data, waiting for the writer's lock (glibc's `_IO_flush_all`): with the slow reader the process
exits 3 after about 1.6 s and the child gets every byte; with `never` it waits for good. Both
translators follow native (LB-29 covers only a holder blocked reading). -/
def slowReader (out : String) : String :=
  "import sys, time\nn = 0\nwhile True:\n    b = sys.stdin.buffer.read1(65536)\n    if not b: break\n    n += len(b)\n    time.sleep(0.05)\nopen('" ++ out ++ "', 'w').write(str(n))\n"

def main (args : List String) : IO Unit := do
  let mode := args[0]!
  let size := args[1]!.toNat!
  let out := args[2]!
  let (cmd, cargs) := if mode == "slow" then ("python3", #["-c", slowReader out]) else ("sleep", #["30"])
  let child ← IO.Process.spawn { cmd := cmd, args := cargs, stdin := .piped }
  let (stdin, _child) ← child.takeStdin
  let _t ← IO.asTask (prio := .dedicated) do
    stdin.write (ByteArray.mk (Array.replicate size 120))
    stdin.flush
  IO.sleep 300
  IO.println s!"exiting with 3 ({size} bytes being written)"
  (← IO.getStdout).flush
  IO.Process.exit 3
