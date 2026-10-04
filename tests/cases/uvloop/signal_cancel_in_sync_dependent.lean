import Std.Internal.UV.Timer
import Std.Internal.UV.Signal
open Std.Internal.UV

/-! LB-20 candidate: a dependent of a one-shot timer's or signal's promise stops or
cancels that timer or signal. With `sync := true` the dependent runs inside
handle_timer_event / handle_signal_event, on the loop thread, before the callback
has marked the handle finished; with `sync := false` it runs on a worker after the
callback (control).
args: KIND (timer | signal) OP (stop | cancel) MODE (sync | async) AFTER (next | reset | none) -/

def say (s : String) : IO Unit := do
  IO.println s
  (← IO.getStdout).flush

def main (args : List String) : IO Unit := do
  let kind := args[0]!
  let op := args[1]!
  let sync := args[2]! == "sync"
  let after := args[3]!
  if kind == "timer" then
    let t ← Timer.mk 10 false
    let p ← t.next
    let tk ← IO.mapTask (sync := sync) (fun _ => if op == "stop" then t.stop else t.cancel) p.result?
    let r ← IO.wait tk
    say s!"dependent ran: {match r with | .ok _ => "ok" | .error e => toString e}"
    say s!"first promise resolved: {← IO.hasFinished p.result?}"
    match after with
    | "next" =>
      for _ in [0:3] do
        let q ← t.next
        say s!"next: {← IO.hasFinished q.result?}"
    | "reset" => t.reset; say "reset: ok"
    | _ => pure ()
  else
    let s ← Signal.mk 10 false  -- SIGUSR1, one-shot
    let p ← s.next
    let tk ← IO.mapTask (sync := sync) (fun _ => if op == "stop" then s.stop else s.cancel) p.result?
    let _ ← IO.Process.output { cmd := "kill", args := #["-USR1", toString (← IO.Process.getPID)] }
    let r ← IO.wait tk
    say s!"dependent ran: {match r with | .ok _ => "ok" | .error e => toString e}"
    say s!"first promise resolved: {← IO.hasFinished p.result?}"
    match after with
    | "next" =>
      for _ in [0:3] do
        let q ← s.next
        say s!"next: {← IO.hasFinished q.result?}"
    | _ => pure ()
  say "done"
