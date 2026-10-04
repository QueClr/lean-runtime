-- `Promise.result!` on a promise dropped without ever being resolved. The
-- drop resolves `result?` with `none` (`deactivate_promise`), and the
-- `sync := true` dependent runs `IO.Option.getOrBlock!`
-- (`lean_option_get_or_block`) there and then, on the dropping thread, here
-- `main`'s: Lean's panic with `force_stderr`, on `std::cerr`, which flushes
-- C's `stdout` first (so "before" is written), then the thread sleeps
-- forever, and the process hangs.

def main (_args : List String) : IO Unit := do
  IO.println "before"
  let p ← IO.Promise.new (α := Nat)
  -- `p`'s last use: compiled Lean drops it right after `result!`
  let t := p.result!
  IO.eprintln s!"not reached: {← IO.hasFinished t}"
