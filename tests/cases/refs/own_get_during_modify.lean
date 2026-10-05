-- The taker's own `get` during its `modify` (review RS4-01; the design of
-- lean-runtime's wait cores, 4.2). `r.modify f` takes the pair out of `r`,
-- so `r` is empty while `f` runs. Here `f` replaces the pair, so the old
-- pair, and with it `some p`, the last reference to the unresolved promise
-- `p`, is freed between the take and the store. Dropping it resolves `p`
-- with `none` (`deactivate_promise`), and `p`'s `sync` dependent runs there
-- and then, on the thread that runs the `modify`, while `r` is taken. It
-- signals `main` (`reached`), then reads `r`. `r` is captured by tasks'
-- closures, so it is multi-threaded natively: the read spins until
-- `modify`'s store, which never comes (the store follows the free, and the
-- free waits for the dependent). That thread hangs; `main` prints and ends
-- the process (`IO.Process.exit 0`). A runtime whose taker's own read did
-- not wait would print the dependent's line with a value never stored.

def main (args : List String) : IO Unit := do
  let n := args[0]!.toNat!
  let p ← IO.Promise.new (α := Nat)
  let reached ← IO.Promise.new (α := Unit)
  let r ← IO.mkRef (n, some p)
  let _d ← IO.mapTask (t := p.result?) (sync := true) fun v => do
    reached.resolve ()
    let (k, _) ← r.get
    IO.eprintln s!"dependent: promise {v}, reference holds {k}"
  let _t ← IO.asTask (prio := .dedicated) do
    IO.eprintln "modify"
    r.modify fun (k, _) => (k + 1, none)
    IO.eprintln s!"after modify: {(← r.get).1}"
  let _ ← IO.wait reached.result?
  IO.eprintln "main: the dependent waits for the reference"
  IO.Process.exit 0
