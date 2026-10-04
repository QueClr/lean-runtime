-- stdout reaches the descriptor in the same chunks as glibc's stdio
-- (`st_blksize` blocks, whole blocks of large writes written directly), so
-- stdout and the unbuffered stderr interleave identically when both go to one
-- pipe (.pipe). Counts and sizes from argv (from lean2rr's
-- tests/runtime/RtBufOrder.lean).

def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let every := args[1]!.toNat!
  let big := args[2]!.toNat!
  let m := args[3]!.toNat!
  let step := args[4]!.toNat!
  let modulus := args[5]!.toNat!
  for i in [0:n] do
    IO.println s!"line {i}"
    if i % every == 0 then IO.eprintln s!"ERR {i}"
  IO.print ("x".pushn 'y' big)
  IO.eprintln "ERR big"
  IO.println ""
  for i in [0:m] do
    IO.print ("z".pushn 'w' (i * step % modulus))
    IO.eprintln s!"ERR {i}"
  let out ← IO.getStdout
  out.putStr "before flush"
  out.flush
  IO.eprintln "ERR after flush"
  IO.println "end"
