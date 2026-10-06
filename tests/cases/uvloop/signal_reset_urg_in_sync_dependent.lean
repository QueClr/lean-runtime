import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A one-shot watcher A of the signal of argv (its Lean number, then its
name for `kill`); a dependent of A's promise (sync or not, argv) starts a
new one-shot watcher B. Two signals: the first one to A, the second one
while B listens. libuv: B, started inside A's callback (sync), finds A
still in the signal tree, so the handler is not registered again after
SA_RESETHAND, and the second signal takes its default action: for SIGURG
and SIGWINCH (default: ignore) it is lost, and B never gets it ("B got:
false", status 0); for SIGUSR1 it ends the program (138). With an async
dependent, A has stopped when B starts, so B's start registers the handler
again and B gets the second signal (true). Review AR-50: lean-runtime's B
got the ignored signal. -/

def kill (sig : String) : IO Unit := do
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #[s!"-{sig}", toString pid] }

def main (args : List String) : IO Unit := do
  let sync := args[0]! == "sync"
  let num := args[1]!.toInt!.toInt32
  let name := args[2]!
  let a ← Signal.mk num false
  let pa ← a.next
  let tb ← IO.mapTask (sync := sync) (fun _ => do
      let b ← Signal.mk num false
      let pb ← b.next
      return (b, pb)) pa.result?
  kill name
  let r ← IO.wait tb
  match r with
  | .ok (b, pb) =>
    IO.println "B listening"
    (← IO.getStdout).flush
    kill name
    IO.sleep 300
    IO.println s!"B got: {← IO.hasFinished pb.result?}"
    b.stop
  | .error e => IO.println s!"error {e}"
