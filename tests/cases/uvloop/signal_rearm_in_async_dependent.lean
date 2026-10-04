import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A one-shot SIGUSR1 watcher A; a dependent of A's promise (sync or not,
argv) starts a new one-shot watcher B and waits for it. Two SIGUSR1 200 ms
apart. libuv: B started inside A's callback (sync) finds A still in the
signal tree, so the handler is not re-registered after SA_RESETHAND, and
the second signal ends the program (138); with an async dependent, B gets
it (review RSIOB-13, the mirroring of SA_RESETHAND). -/

def kill (sig : String) : IO Unit := do
  let pid ← IO.Process.getPID
  let _ ← IO.Process.output { cmd := "kill", args := #[s!"-{sig}", toString pid] }

def main (args : List String) : IO Unit := do
  let sync := args[0]! == "sync"
  let a ← Signal.mk 10 false
  let pa ← a.next
  let tb ← IO.mapTask (sync := sync) (fun _ => do
      let b ← Signal.mk 10 false
      let pb ← b.next
      return (b, pb)) pa.result?
  kill "USR1"
  let r ← IO.wait tb
  match r with
  | .ok (_, pb) =>
    IO.println "B listening"
    (← IO.getStdout).flush
    kill "USR1"
    IO.println s!"B got {repr (← IO.wait pb.result?)}"
  | .error e => IO.println s!"error {e}"
