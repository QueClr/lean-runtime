import Std.Internal.UV.Signal
open Std.Internal.UV

/-! LB-34, `cancel` of a one-shot watcher (no signal is sent). The `sync`
dependent of its promise runs inside `cancel` (the watcher holds the
promise's last reference); it calls `next` once and keeps the promise it
gets. Then `main` makes COUNT fresh promises, and prints whether one of
them has the kept promise's address (`aliased`), whether the kept one has
resolved, and again after it resolves every fresh promise. Correct
outcome: the watcher is made initial first (lean-runtime's order; Lean master's `cancel` is unchanged), so the dependent's `next` starts it listening again and gives its new promise, which no signal resolves.
Native 4.34.0: `next` gives the promise being freed; a fresh promise
takes its memory, so the kept reference sees that promise resolve.
args: OP (stop | cancel) SIGNAL (Lean's number) COUNT -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

unsafe def addrImpl (p : IO.Promise Int) : USize := ptrAddrUnsafe p
@[implemented_by addrImpl] opaque addr (p : IO.Promise Int) : USize

def main (args : List String) : IO Unit := do
  let op := args[0]!
  let num := args[1]!.toInt!
  let count := args[2]!.toNat!
  let kept ← IO.mkRef (none : Option (IO.Promise Int))
  let s ← Signal.mk (Int32.ofInt num) false
  let p ← s.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      let q ← s.next
      say s!"dependent: value {if v.isSome then "some" else "none"}"
      kept.set (some q)) p.result?
  say s!"{op}: begin"
  if op == "cancel" then s.cancel else s.stop
  say s!"{op}: end"
  let mut fresh : Array (IO.Promise Int) := #[]
  for _ in [0:count] do
    fresh := fresh.push (← IO.Promise.new)
  let some q ← kept.get | do say "nothing kept"; IO.Process.exit 0
  let a := addr q
  say s!"aliased: {fresh.any (addr · == a)}"
  say s!"kept resolved: {← q.isResolved}"
  for f in fresh do
    f.resolve 7
  say s!"kept resolved after resolving the fresh promises: {← q.isResolved}"
  -- no release of the kept reference at the end (natively it points at
  -- freed memory)
  IO.Process.exit 0
