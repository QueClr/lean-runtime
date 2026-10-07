import Std.Internal.UV.Timer
open Std.Internal.UV

/-! libuv starts a repeating timer's next period from the loop's time of the
iteration that fires it (`uv_timer_again`), not from when its callback runs.
A repeating timer of period 1 s: its tick 1 has a `sync` dependent that
takes the promise of tick 2 (the dependents print nothing: in the
single-thread scheduler a print there is an effect point, which would let
`main` go first). A task X is sleeping (300 ms) when `main` starts a
computation of about 1.5 s without scheduling points; X then computes for
about 2 s. Natively tick 1 fires at 1 s and tick 2 at 2 s, on the loop
thread, while `main` and X compute; `main` looks at 3.2 s from its start:
"tick 2 by then: true". In the single-thread scheduler, `main`'s clock read
after its computation finds tick 1 due and X's sleep over: X runs first
(2 s), then the loop context runs tick 1. Before review HU-06 (fixes-14) its
next period started then, so tick 2 came about 3 s after the look, after
`main`'s check; it starts from the look now, as natively from the
iteration's time. `main` first counts the rounds of `spin` per 100 ms in
this build (`calibrate`). The arguments: the length of `calibrate`'s chunks,
then the twin's two computations, in ms. -/

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
  let lenX ← IO.mkRef (20 * rounds)
  let t ← Timer.mk 1000 true
  let p0 ← t.next
  let _ ← IO.wait p0.result?
  let t0 ← IO.monoMsNow
  let p1 ← t.next
  let tick2 ← IO.mkRef (none : Option (Task (Option Unit)))
  let _ ← IO.mapTask (sync := true) (t := p1.result?) fun _ => do
    let p2 ← t.next
    tick2.set (some p2.result?)
  -- X reads its length before it sleeps: no scheduling point after the sleep
  let ready ← IO.Promise.new (α := Unit)
  let xt ← IO.asTask do
    let n ← lenX.get
    ready.resolve ()
    IO.sleep 300
    return spin n x == 0
  let _ ← IO.wait ready.result?
  -- about 1.5 s without a scheduling point: tick 1 comes due meanwhile
  let y := spin (15 * rounds) x
  if y == 0 then IO.println "never"
  IO.sleep (t0 + 3200 - (← IO.monoMsNow)).toUInt32
  let f ← match ← tick2.get with
    | some t2 => IO.hasFinished t2
    | none => pure false
  IO.println s!"tick 2 by then: {f}"
  IO.println s!"X: {← IO.wait xt}"
