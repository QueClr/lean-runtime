-- The order in which a caller releases the handles it lent to a call.
-- Natively Lean's RC pass puts a `dec` after the call for each dead argument
-- passed to a borrowed parameter: it visits the arguments in order, takes
-- each variable at its first occurrence, and prepends the `dec`, so the
-- handles are released last argument first. Several append handles on one
-- file show the order: each handle's buffered text reaches the file when it
-- is closed.
-- - `put3 a b c` with three dead handles;
-- - the same handle twice (`put4 w h1 h2 h1`);
-- - a handle passed first to an owned parameter, then to a borrowed one
--   (`own3 x y x`): its first occurrence decides;
-- - a function value (`apply3 put3 a b c`): `put3._boxed` releases its
--   borrowed arguments after the call the same way.
-- The texts come from the command line.

@[noinline] def put3 (a b c : IO.FS.Handle) (s : Array String) : IO Unit := do
  a.putStr s[0]!; b.putStr s[1]!; c.putStr s[2]!

@[noinline] def put4 (w h1 h2 h3 : IO.FS.Handle) (s : Array String) : IO Unit := do
  w.putStr s[0]!; h1.putStr s[1]!; h2.putStr s[2]!; h3.putStr s[3]!

-- `a` is stored in a reference, so Lean takes it owned; `b` and `c` are
-- only written, so Lean borrows them.
@[noinline] def own3 (a b c : IO.FS.Handle) (s : Array String) : IO Unit := do
  let r ← IO.mkRef a
  b.putStr s[1]!; c.putStr s[2]!
  (← r.get).putStr s[0]!

@[noinline] def apply3 (f : IO.FS.Handle → IO.FS.Handle → IO.FS.Handle → Array String → IO Unit)
    (a b c : IO.FS.Handle) (s : Array String) : IO Unit :=
  f a b c s

def fresh (name : String) : IO IO.FS.Handle := do
  IO.FS.writeFile name ""
  IO.FS.Handle.mk name .append

def main (args : List String) : IO Unit := do
  let s := args.toArray
  let a ← fresh "o1.txt"
  let b ← IO.FS.Handle.mk "o1.txt" .append
  let c ← IO.FS.Handle.mk "o1.txt" .append
  put3 a b c s
  IO.println s!"put3: {← IO.FS.readFile "o1.txt"}"
  let w ← fresh "o2.txt"
  let h1 ← IO.FS.Handle.mk "o2.txt" .append
  let h2 ← IO.FS.Handle.mk "o2.txt" .append
  put4 w h1 h2 h1 s
  IO.println s!"put4 w h1 h2 h1: {← IO.FS.readFile "o2.txt"}"
  let x ← fresh "o3.txt"
  let y ← IO.FS.Handle.mk "o3.txt" .append
  own3 x y x s
  IO.println s!"own3 x y x: {← IO.FS.readFile "o3.txt"}"
  let a ← fresh "o4.txt"
  let b ← IO.FS.Handle.mk "o4.txt" .append
  let c ← IO.FS.Handle.mk "o4.txt" .append
  apply3 put3 a b c s
  IO.println s!"apply3 put3: {← IO.FS.readFile "o4.txt"}"
