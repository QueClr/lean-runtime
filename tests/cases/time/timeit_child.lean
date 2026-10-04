/-! V11 row group: `timeit` (chapter 05 section 3.5's timing rows). Its line on standard error holds a
measured time, so the fixture runs itself as a child (`IO.appPath`) with standard error piped and prints
only whether the child's line has `timeit`'s shape: the message, a space, a number with at most three
significant digits, and `ms` (or `s` from one second on). -/

def shapeOk (line : String) (msg : String) : Bool :=
  if !line.startsWith (msg ++ " ") then false else
  let rest := (line.drop (msg.length + 1)).toString
  let rest := if rest.endsWith "\n" then (rest.dropEnd 1).toString else rest
  let num := if rest.endsWith "ms" then (rest.dropEnd 2).toString else if rest.endsWith "s" then (rest.dropEnd 1).toString else "x"
  let mantissa := (num.splitOn "e").headD ""
  let significant := (mantissa.toList.filter Char.isDigit).dropWhile (· == '0')
  !num.isEmpty && num.toList.all (fun c => c.isDigit || c == '.' || c == 'e' || c == '-' || c == '+') &&
    significant.length ≤ 3

def main (args : List String) : IO UInt32 := do
  match args with
  | ["child"] =>
    let r ← timeit "label one" (pure 41)
    IO.println s!"child result {r + 1}"
    let r ← timeit "label two" (do IO.sleep 20; pure "slept")
    IO.println s!"child result {r}"
    try
      let _ ← timeit "label three" (throw (IO.userError "inside") : IO Nat)
    catch e =>
      IO.println s!"child caught {e}"
    return 5
  | _ =>
    let o ← IO.Process.output { cmd := (← IO.appPath).toString, args := #["child"] }
    IO.println s!"child exit {o.exitCode}"
    IO.println s!"child stdout {repr o.stdout}"
    let lines := o.stderr.splitOn "\n" |>.filter (· ≠ "")
    IO.println s!"stderr lines {lines.length}"
    for (l, m) in lines.zip ["label one", "label two", "label three"] do
      IO.println s!"{m}: {shapeOk l m}"
    return 0
