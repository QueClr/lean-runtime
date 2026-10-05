-- The order of the `sync` dependents of promises dropped by nested frees
-- (the design of lean-runtime's wait cores, 5.6; core 3.3, R5). A free drops
-- promise `pA` (and, in part 2, `pB` after it). `pA`'s `sync` dependent
-- frees an array that holds promise `pC` (it sets the reference that holds
-- it to `#[]`), then prints `A`. `pB`'s and `pC`'s dependents print `B` and
-- `C`. Natively a `sync` dependent runs inside the free, when the free
-- reaches its promise (`deactivate_promise` -> `resolve_core` ->
-- `handle_finished` -> `enqueue_core` at `LEAN_SYNC_PRIO`), and the free
-- inside a dependent is a new `lean_dec_ref_cold`: so `C` before `A`, and
-- `A` before `B`. The arrays are freed from their last element, so
-- `#[pB, pA]` frees `pA` first. A runtime that resolves a free's promises
-- after the free must resolve those of a free inside a dependent before the
-- outer free's next promise: `pC` before `pB` if and only if `pA` before
-- `pB`. Every dependent runs outside the free (each prints, a ref write).

/-- `pA`'s dependent: frees the array in `inner` (which holds `pC`), then
prints `A`. `pC`'s dependent prints `C`. -/
def setup (inner : IO.Ref (Array (IO.Promise Unit))) (pA : IO.Promise Unit) : IO Unit := do
  let pC ← IO.Promise.new (α := Unit)
  let _ ← IO.mapTask (sync := true) (t := pC.result?) fun _ => IO.println "  C"
  inner.set #[pC]
  let _ ← IO.mapTask (sync := true) (t := pA.result?) fun _ => do
    inner.set #[]
    IO.println "  A"

def part1 : IO Unit := do
  IO.println "part 1 (one array: pA)"
  let inner ← IO.mkRef #[]
  let pA ← IO.Promise.new (α := Unit)
  setup inner pA
  let outer ← IO.mkRef #[pA]
  outer.set #[]
  IO.println "  after the free"

def part2 : IO Unit := do
  IO.println "part 2 (one array: pB, pA)"
  let inner ← IO.mkRef #[]
  let pA ← IO.Promise.new (α := Unit)
  let pB ← IO.Promise.new (α := Unit)
  setup inner pA
  let _ ← IO.mapTask (sync := true) (t := pB.result?) fun _ => IO.println "  B"
  let outer ← IO.mkRef #[pB, pA]
  outer.set #[]
  IO.println "  after the free"

def main (_args : List String) : IO Unit := do
  part1
  part2
