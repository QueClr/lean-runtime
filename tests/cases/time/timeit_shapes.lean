/-! Chapter 06 A4 fixture A752 (kind `io`): `timeit` over an action passed as a value, not a single-use
inline closure: a let-bound action used twice, an action parameter, an action read from an array, a
nested `timeit`, and `timeit` of a call taking the action. The row's runtime function takes an
`impl FnOnce`, and the action value reached it as its class value `Rc<dyn Fn() -> ..>` (rustc E0277);
it is now passed as at a translated callee's arrow parameter (chapter 03 F2). Found by the breaker of
sysio (eac34312, T/Timeit2.lean, T/Timeit.lean). The lines `timeit` writes hold measured times, so they
are captured (`withIsolatedStreams`) and printed by shape. -/
@[noinline] def viaParam (act : IO Nat) : IO Nat := timeit "param" act

def shape (line : String) : String :=
  match line.splitOn " " with
  | [msg, num] => s!"{msg} <{if num.endsWith "ms" then "ms" else if num.endsWith "s" then "s" else "?"}>"
  | _ => s!"other: {line}"

def main (args : List String) : IO Unit := do
  let n := args.length
  let (err, r) ← IO.FS.withIsolatedStreams (do
    let act : IO Nat := pure (7 + n)
    let a ← timeit "twice-a" act
    let b ← timeit "twice-b" act
    let c ← viaParam (pure (9 + n))
    let acts : Array (IO Nat) := #[pure (1 + n), pure (2 + n)]
    let d ← timeit "from-array" (match acts[1]? with | some x => x | none => act)
    let e ← timeit "once" (do IO.eprintln "inside once"; pure (5 + n))
    let f ← timeit "ref-param" (viaParam act)
    let g ← timeit "outer" (do
      let x ← timeit "inner" (do IO.eprintln "inside"; pure 1)
      let y ← timeit "inner2" (pure 2)
      pure (x + y))
    return s!"{a} {b} {c} {d} {e} {f} {g}" : IO String)
  IO.println r
  for l in err.splitOn "\n" |>.filter (· ≠ "") do IO.println (shape l)
