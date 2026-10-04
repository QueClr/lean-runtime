-- A dedicated task blocks in `IO.waitAny [x]`, `x` a pure `Task.map` of
-- `r`. `r`'s walk queues `x` (newer), then runs an older `sync` dependent
-- that never returns (a sleep loop, documented misuse). Natively a worker
-- runs `x`, and its finish wakes `waitAny`; here the enqueue lets `waitAny`
-- run `x` itself. `main` stays in the walk: the program hangs. (Review of
-- sched-2, RS2-09; probe WaitAnyPureStalled.)

partial def spin : IO Unit := do
  IO.sleep 100
  spin

def main (_args : List String) : IO Unit := do
  let p ← IO.Promise.new (α := Nat)
  let r := p.result?
  let _s ← IO.mapTask (sync := true) (fun _ => spin) r
  let x := r.map (fun o => o.getD 0 + 1)
  let w ← IO.asTask (prio := .dedicated) do
    let v ← IO.waitAny [x]
    IO.eprintln s!"waitAny woke: {v}"
  IO.sleep 100
  IO.eprintln "resolving"
  p.resolve 1
  IO.eprintln s!"not reached {(← IO.wait w).isOk}"
