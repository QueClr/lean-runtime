/-! `allocprof msg act` (`lean_io_allocprof`, io.cpp): runs `act`, then prints `msg` up to its
first NUL byte and a note on the current standard error, with `io_eprintln`'s newline after them,
also when `act` fails. Lean 4.34.0's release runtime is built without `LEAN_RUNTIME_STATS`, so the
note says no data is available (`allocprof.cpp`). -/

def main (args : List String) : IO Unit := do
  let r ← allocprof "profile\u0000hidden" (do IO.eprintln "inside"; pure (args.length + 1))
  IO.println s!"allocprof {r}"
  try
    let _ ← (allocprof s!"failing {args.length}" (throw (IO.userError "inner")) : IO Nat)
    IO.println "not reached"
  catch e => IO.println s!"caught: {e}"
