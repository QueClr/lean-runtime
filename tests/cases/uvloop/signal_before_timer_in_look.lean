import Std.Internal.UV.Timer
import Std.Internal.UV.Signal
open Std.Internal.UV

/-! Within one iteration libuv runs the io callbacks (a signal watcher's
pipe among them) before the timers due. A one-shot timer A of 10 ms has a
`sync` dependent that computes for about 1 s on the loop thread, then
prints. Meanwhile a one-shot timer B of 100 ms comes due, and a child sends
SIGUSR1 at about 300 ms, which a one-shot watcher listens for; each has a
`sync` dependent that prints. When A's dependent returns, the loop finds
both in one look: natively "A done", "signal", "timer B", though B came due
first. Before review HU-04 (fixes-14) the single-thread scheduler queued
the timers due before the descriptors' events within one look: "timer B"
came before "signal". `main` first counts the rounds of `spin` per 100 ms
in this build (`calibrate`). The arguments: the signal number, the length
of `calibrate`'s chunks, and the twin's computation, in ms. -/

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
  let signum := Int32.ofInt args[0]!.toInt!
  let chunk := args[1]!.toNat!.toUInt64
  let seed := (← IO.monoNanosNow).toUInt64 ||| 1
  let (rounds, x) ← calibrate chunk (← IO.monoMsNow) 0 seed
  let len ← IO.mkRef (10 * rounds)
  let w ← Signal.mk signum false
  let pw ← w.next
  let _ ← IO.mapTask (sync := true) (t := pw.result?) fun _ => IO.println "signal"
  let ta ← Timer.mk 10 false
  let pa ← ta.next
  let _ ← IO.mapTask (sync := true) (t := pa.result?) fun _ => do
    let y := spin (← len.get) x
    IO.println s!"A done {y == 0}"
  let tb ← Timer.mk 100 false
  let pb ← tb.next
  let _ ← IO.mapTask (sync := true) (t := pb.result?) fun _ => IO.println "timer B"
  let pid ← IO.Process.getPID
  let _child ← IO.Process.spawn { cmd := "sh", args := #["-c", s!"sleep 0.3; kill -USR1 {pid}"] }
  IO.sleep 2000
  IO.println "main done"
