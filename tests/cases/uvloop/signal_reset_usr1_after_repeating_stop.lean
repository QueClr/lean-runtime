import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A repeating watcher W of the signal of argv (its Lean number, then its
name for `kill`) gets a first signal. A sync dependent of its promise, on
the loop: a one-shot watcher O starts, with a sync dependent of O's promise
that starts a one-shot watcher B; a child sends a second signal while the
dependent computes (no IO; the loop is busy); then W stops, so only O is
left and libuv registers the handler again, with SA_RESETHAND. A third
signal once B listens. libuv: the second signal came before that
registration, under W's, which has no SA_RESETHAND; the loop delivers it to
O once the dependent returns, and B, started in O's callback, finds O still
in the signal tree and registers nothing. The SA_RESETHAND registration has
had no signal, so the third one is caught and B gets it ("B got: true",
status 0, for SIGUSR1 and SIGURG alike). The third argument is the native
busy loop's length; the ports spin for the fourth, in milliseconds. Review
AR-50, part 2: lean-runtime's loop took the second signal after the
registration and counted it as the registration's own, so it dropped the
third one ("B got: false"). -/

def busy (n : Nat) : Nat := Id.run do
  let mut acc := 0
  for i in [0:n] do
    acc := (acc + i * 7) % 1000003
  return acc

def kill (sig : String) : IO Unit := do
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #[s!"-{sig}", toString pid] }

def main (args : List String) : IO Unit := do
  let num := args[0]!.toInt!.toInt32
  let name := args[1]!
  let n := args[2]!.toNat!
  let pid ← IO.Process.getPID
  let w ← Signal.mk num true
  let pw ← w.next
  let o ← Signal.mk num false
  let tt ← IO.mapTask (sync := true) (fun _ => do
      let po ← o.next
      let tb ← IO.mapTask (sync := true) (fun _ => do
          let b ← Signal.mk num false
          let pb ← b.next
          return (b, pb)) po.result?
      let _ ← IO.Process.spawn { cmd := "sh", args := #["-c", s!"sleep 0.1; kill -{name} {pid}"] }
      if busy n == 1000004 then IO.println "never"
      w.stop
      return tb) pw.result?
  kill name
  let r ← match ← IO.wait tt with
    | .ok tb => IO.wait tb
    | .error e => pure (.error e)
  match r with
  | .ok (b, pb) =>
    IO.println "B listening"
    (← IO.getStdout).flush
    kill name
    IO.sleep 300
    IO.println s!"B got: {← IO.hasFinished pb.result?}"
    b.stop
  | .error e => IO.println s!"error {e}"
