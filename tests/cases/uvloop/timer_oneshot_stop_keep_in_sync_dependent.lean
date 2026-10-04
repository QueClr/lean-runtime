import Std.Internal.UV.Timer
open Std.Internal.UV

/-! LB-33, `stop` of a one-shot timer before it fires. The `sync` dependent of
its promise runs inside `stop` (the timer holds the promise's last
reference); it calls `next` once and keeps the promise it gets. Then
`main` makes COUNT fresh promises, and prints whether one of them has the
kept promise's address (`aliased`), whether the kept one has resolved,
and again after it resolves every fresh promise. Correct outcome: the timer is finished first (Lean master, PR #14793), so the dependent's `next` gives a new promise that the stopped timer does not hold; the program holds it, so it stays pending.
Native 4.34.0: `next` gives the promise being freed; a fresh promise
takes its memory, so the kept reference sees that promise resolve.
args: OP (stop | cancel) TIMEOUT (ms) COUNT -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

unsafe def addrImpl (p : IO.Promise Unit) : USize := ptrAddrUnsafe p
@[implemented_by addrImpl] opaque addr (p : IO.Promise Unit) : USize

def main (args : List String) : IO Unit := do
  let op := args[0]!
  let ms := args[1]!.toNat!
  let count := args[2]!.toNat!
  let kept ← IO.mkRef (none : Option (IO.Promise Unit))
  let t ← Timer.mk ms.toUInt64 false
  let p ← t.next
  let _ ← IO.mapTask (sync := true) (fun v => do
      let q ← t.next
      say s!"dependent: value {if v.isSome then "some" else "none"}"
      kept.set (some q)) p.result?
  say s!"{op}: begin"
  if op == "cancel" then t.cancel else t.stop
  say s!"{op}: end"
  let mut fresh : Array (IO.Promise Unit) := #[]
  for _ in [0:count] do
    fresh := fresh.push (← IO.Promise.new)
  let some q ← kept.get | do say "nothing kept"; IO.Process.exit 0
  let a := addr q
  say s!"aliased: {fresh.any (addr · == a)}"
  say s!"kept resolved: {← q.isResolved}"
  for f in fresh do
    f.resolve ()
  say s!"kept resolved after resolving the fresh promises: {← q.isResolved}"
  -- no release of the kept reference at the end (natively it points at
  -- freed memory)
  IO.Process.exit 0
