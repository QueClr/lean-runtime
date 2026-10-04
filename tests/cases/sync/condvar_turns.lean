import Std.Sync
open Std

-- Two tasks on threads of their own take turns through a `BaseMutex` and a
-- `Condvar`: each waits until it is its turn, so the log alternates. One of
-- them is waited for first by `main` (here it runs on `main`'s stack, as a
-- task that is needed does, and blocks there). After lean2rr's
-- RtSyncCondvar.

def player (m : BaseMutex) (cv : Condvar) (turn : IO.Ref Nat) (me rounds : Nat)
    (log : IO.Ref (Array String)) : IO Unit := do
  for i in [0:rounds] do
    m.lock
    while (← turn.get) % 2 != me do
      cv.wait m
    log.modify (·.push s!"player {me} round {i}")
    turn.modify (· + 1)
    m.unlock
    cv.notifyAll

def main (args : List String) : IO Unit := do
  let rounds := args.head!.toNat!
  let m ← BaseMutex.new
  let cv ← Condvar.new
  let turn ← IO.mkRef 0
  let log ← IO.mkRef #[]
  let a ← IO.asTask (prio := .dedicated) (player m cv turn 1 rounds log)
  let b ← IO.asTask (prio := .dedicated) (player m cv turn 0 rounds log)
  let _ ← IO.wait a
  let _ ← IO.wait b
  for l in ← log.get do IO.println l
