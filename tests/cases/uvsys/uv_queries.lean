import Std.Internal.UV.System
/-! Chapter 06 A4 fixture A756 (kind `io`): three build failures over `Std.Internal.UV.System`'s rows and
mirrors, found by the breaker of sysio-uv (02cb35a5, V/Sys.lean, V/Mem.lean, V/Home.lean, V/Upd.lean).
(1) A zero-arity query passed as an action value printed `|| io::os_gethostname()`, clippy's
`redundant_closure`: it is the path `io::os_gethostname` (chapter 03 L8). (2) A mirror's `Option String`
field reached the program's `Option<Rc<Str>>` without its conversion: a function returning the field
of a borrowed mirror was planned as a reference into it (E0308; chapter 04 B5 declines a chain through a
mirror), and a rebuild of the mirror wrote `Rc::unwrap_or_clone` over an `Option` (chapter 03 T1). (3)
`RUsage`, all of whose fields are `UInt64`, was planned `Copy` as chapter 03 T8 derives a generated type,
but the runtime's struct was only `Clone`, so a value passed by value and read again was a use after
move (E0382): a mirror derives what T8 gives its layout. -/

open Std.Internal.UV.System

def tryP {α} [ToString α] (label : String) (x : IO α) : IO Unit := do
  try IO.println s!"{label}: {← x}" catch e => IO.println s!"{label}: error {e}"

@[noinline] def nonEmpty (label : String) (act : IO String) : IO Unit := do
  try IO.println s!"{label}: {decide ((← act).length > 0)}" catch e => IO.println s!"{label}: error {e}"
@[noinline] def positive (label : String) (act : IO UInt64) : IO Unit := do
  try IO.println s!"{label}: {decide ((← act) > 0)}" catch e => IO.println s!"{label}: error {e}"
@[noinline] def same (a b : Option String) : Bool := a == b
@[noinline] def pick (p : PasswdInfo) : Option String := p.homedir
@[noinline] def bumpR (r : RUsage) : RUsage := { r with maxRSS := 0, signals := 7 }

def main : IO Unit := do
  -- (1) zero-arity queries as action values
  nonEmpty "hostname" osGetHostname
  nonEmpty "tmpdir" osTmpdir
  positive "total" totalMemory
  positive "pid" osGetPid
  let act : IO UInt64 := totalMemory
  positive "total again" act
  positive "total once more" act
  -- (2) Option String fields
  let p ← osGetPasswd
  let h ← osHomedir
  IO.println s!"{decide (some h == p.homedir)} {same p.homedir p.homedir} {same (pick p) p.homedir}"
  let p2 := { p with homedir := some h, shell := p.homedir }
  IO.println s!"{decide (p2.homedir == p.homedir)} {decide (p2.shell == p.homedir)} {p2.username == p.username}"
  let q : PasswdInfo := ⟨"u", some 1, none, some h, p.shell⟩
  IO.println s!"{q.username} {decide (q.homedir == some h)}"
  -- (3) RUsage passed by value and read again
  let r ← getrusage
  let r2 := bumpR r
  IO.println s!"{r2.maxRSS} {r2.signals} {decide (r2.userTime == r.userTime)}"
