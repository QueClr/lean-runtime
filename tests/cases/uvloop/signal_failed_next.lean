import Std.Internal.UV.Signal
open Std.Internal.UV

/-! A signal watcher of a number Lean does not know: `next` fails with
`EINVAL`; `cancel`, then `next` again, fails again; `stop` ends it. LB-19:
native's failed `next` releases the watcher once too often, and the
`cancel` releases it again while the program holds it, so the second `next`
uses freed memory (SIGSEGV). The number comes from argv. -/

def main (args : List String) : IO Unit := do
  let s ← Signal.mk (Int32.ofInt args[0]!.toInt!) false
  match ← s.next.toBaseIO with
  | .ok _ => IO.println "first next: ok"
  | .error e => IO.println s!"first next: {e}"
  s.cancel
  IO.println "cancelled"
  match ← s.next.toBaseIO with
  | .ok _ => IO.println "second next: ok"
  | .error e => IO.println s!"second next: {e}"
  s.stop
  IO.println "done"
