import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer of 300 ms, with a `sync` dependent that prints, and no
other event loop work: `main` starts the timer, leaves an IO task that
computes (no IO, so no scheduling point) for about 1.5 s, prints "main
done" and returns. Natively libuv's loop thread fires the timer at 300 ms,
while the task computes, and the exit waits for the task: "main done", then
"timer fired". Before review AR-52 (fixes-14) the single-thread scheduler's
final run ran the task on `main`'s stack and, with no event loop context
alive, never looked at the timers: "timer fired" was lost. `main` first
counts the rounds of `spin` per 100 ms in this build (`calibrate`), and the
task runs 15 times as many. The first argument is the length of
`calibrate`'s chunks; the second is the twin's computation, in ms. -/

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
  let chunk := args[0]!.toNat!.toUInt64
  let seed := (← IO.monoNanosNow).toUInt64 ||| 1
  let (rounds, x) ← calibrate chunk (← IO.monoMsNow) 0 seed
  -- read inside the task, so that the computation stays there
  let len ← IO.mkRef (15 * rounds)
  let tm ← Timer.mk 300 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => IO.println "timer fired"
  let _w ← IO.asTask do
    if spin (← len.get) x == 0 then IO.println "never"
  IO.println "main done"
