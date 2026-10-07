import Std.Internal.UV.Timer
import Std.Internal.UV.Signal
open Std.Internal.UV

/-! Natively a signal handler writes one message per watcher listening when
the signal comes, so a watcher started later does not get that signal. A
repeating watcher W0 of SIGUSR2 listens all along; a one-shot watcher W1 of
SIGUSR1 has a `sync` dependent that starts a one-shot watcher W2 of
SIGUSR2. While the loop thread computes in a `sync` dependent of a timer A
(about 1 s), a child sends SIGUSR1, then SIGUSR2. When A's dependent
returns, the loop delivers SIGUSR1 to W1 (whose dependent starts W2), then
SIGUSR2 to W0 only: W2's promise stays pending. Before review HU-05
(fixes-14) both modes of lean-runtime took a signal's watchers only when
they delivered it, after the earlier signals of the batch, so W2 got
SIGUSR2 too. `main` first counts the rounds of `spin` per 100 ms in this
build (`calibrate`). The arguments: SIGUSR1's and SIGUSR2's numbers, the
length of `calibrate`'s chunks, and the twin's computation, in ms. -/

/-- `n` rounds of xorshift64 from `x` (nonzero): a loop that no build folds
away, unlike an LCG (hunt HU's aside). -/
partial def spin (n x : UInt64) : UInt64 :=
  if n == 0 then x else
    let x := x ^^^ (x <<< 13)
    let x := x ^^^ (x >>> 7)
    let x := x ^^^ (x <<< 17)
    spin (n - 1) x

/-- The rounds of `spin` per 100 ms in this build, and the last value:
`spin` in chunks of `chunk` rounds from `x` until 100 ms have passed, with
clock reads between the chunks; each chunk starts from the last one's
value. -/
partial def calibrate (chunk : UInt64) (t0 : Nat) (rounds x : UInt64) : IO (UInt64 × UInt64) := do
  let x := spin chunk x
  let rounds := rounds + chunk
  if (← IO.monoMsNow) - t0 ≥ 100 then
    return (rounds, x)
  calibrate chunk t0 rounds x

def main (args : List String) : IO Unit := do
  let usr1 := Int32.ofInt args[0]!.toInt!
  let usr2 := Int32.ofInt args[1]!.toInt!
  let chunk := args[2]!.toNat!.toUInt64
  let seed := (← IO.monoNanosNow).toUInt64 ||| 1
  let (rounds, x) ← calibrate chunk (← IO.monoMsNow) 0 seed
  let len ← IO.mkRef (10 * rounds)
  let w0 ← Signal.mk usr2 true
  let p0 ← w0.next
  let _ ← IO.mapTask (sync := true) (t := p0.result?) fun v => IO.println s!"W0 got {v}"
  let w2task ← IO.mkRef (none : Option (Task (Option Int)))
  let w1 ← Signal.mk usr1 false
  let p1 ← w1.next
  let _ ← IO.mapTask (sync := true) (t := p1.result?) fun v => do
    let w2 ← Signal.mk usr2 false
    let p2 ← w2.next
    let _ ← IO.mapTask (sync := true) (t := p2.result?) fun v => IO.println s!"W2 got {v}"
    w2task.set (some p2.result?)
    IO.println s!"W1 got {v}, started W2"
  let ta ← Timer.mk 10 false
  let pa ← ta.next
  let _ ← IO.mapTask (sync := true) (t := pa.result?) fun _ => do
    let y := spin (← len.get) x
    IO.println s!"A done {y == 0}"
  let pid ← IO.Process.getPID
  let _child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", s!"sleep 0.3; kill -USR1 {pid}; sleep 0.1; kill -USR2 {pid}"] }
  IO.sleep 2000
  match ← w2task.get with
  | some t => IO.println s!"W2 finished: {← IO.hasFinished t}"
  | none => IO.println "W2 not started"
