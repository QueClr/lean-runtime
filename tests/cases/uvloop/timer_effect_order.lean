import Std.Internal.UV.Timer
import Std.Async.Timer
open Std.Internal.UV
open Std.Async

/-! The order of an event's output against `main`'s output after a stretch
without scheduling points (a pure computation of about 0.5 s). Phase 1
(control): a task that sleeps 200 ms, already sleeping when `main` starts
computing. Phase 2: a one-shot UV timer of 100 ms, started right before
`main` computes, with a `sync` dependent that prints. Phase 3:
`Std.Async.sleep 100` in an async task, which then prints. Natively the
sleeper, the timer and the async task's timer fire while `main` computes,
so each event's line comes before `main`'s. Before review HU-01 (fixes-14)
the single-thread scheduler let a timer due at an effect point go first
only after 5 ms (`STALE`), counted from the effect point itself, so phases 2
and 3 printed `main`'s line first; a due sleeper already went first.
`main` first counts the rounds of `spin` per 100 ms in this build
(`calibrate`). The first argument is the length of `calibrate`'s chunks;
the second is the twin's computation of each phase, in ms. -/

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
  let n := 5 * rounds
  -- phase 1: a task that is sleeping when `main` starts computing
  let ready ← IO.Promise.new (α := Unit)
  let s ← IO.asTask do
    ready.resolve ()
    IO.sleep 200
    IO.println "phase 1: sleeper woke"
  let _ ← IO.wait ready.result?
  let x1 := spin n x
  IO.println s!"phase 1: main computed {x1 == 0}"
  let _ ← IO.wait s
  -- phase 2: a UV timer
  let t ← Timer.mk 100 false
  let p ← t.next
  let d ← IO.mapTask (t := p.result?) (sync := true) fun _ =>
    IO.println "phase 2: timer fired"
  let x2 := spin n x1
  IO.println s!"phase 2: main computed {x2 == 0}"
  let _ ← IO.wait d
  -- phase 3: `Std.Async.sleep`
  let atk ← (do Std.Async.sleep 100; IO.println "phase 3: async sleep done" : Async Unit).toIO
  let x3 := spin n x2
  IO.println s!"phase 3: main computed {x3 == 0}"
  let r ← IO.wait atk
  IO.println s!"phase 3: async task ok: {r.isOk}"
