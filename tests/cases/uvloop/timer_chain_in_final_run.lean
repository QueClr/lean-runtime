import Std.Internal.UV.Timer
open Std.Internal.UV

/-! A one-shot timer A of 10 ms whose `sync` dependent starts a one-shot
timer B of 700 ms (with a `sync` dependent that prints "B fired") and
prints "A fired". `main` sleeps 300 ms (A fires meanwhile, and the event
loop context ends), leaves an IO task that computes (no IO, so no
scheduling point) for about 1.5 s, prints "main done" and returns. Natively
libuv's loop thread fires B at about 710 ms, while the task computes:
"A fired", "main done", "B fired". Before review AR-52 (fixes-14) the
single-thread scheduler's final run, with no event loop context alive,
never looked at the timers: "B fired" was lost. `main` first counts the
rounds of `spin` per 100 ms in this build (`calibrate`), and the task runs
15 times as many. The first argument is the length of `calibrate`'s
chunks; the second is the twin's computation, in ms. -/

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
  let len ← IO.mkRef (15 * rounds)
  let tm ← Timer.mk 10 false
  let p ← tm.next
  let _dep ← IO.mapTask (sync := true) (t := p.result?) fun _ => do
    let tb ← Timer.mk 700 false
    let pb ← tb.next
    let _ ← IO.mapTask (sync := true) (t := pb.result?) fun _ => IO.println "B fired"
    IO.println "A fired"
  IO.sleep 300
  let _w ← IO.asTask do
    if spin (← len.get) x == 0 then IO.println "never"
  IO.println "main done"
