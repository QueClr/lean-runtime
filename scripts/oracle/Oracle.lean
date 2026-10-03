/-
The row oracle of lean-runtime: a native Lean 4.34.0 program that evaluates
the functions of `tests/cases/*.rows` on inputs read from stdin, so that no
input is a literal the compiler could fold.

`scripts/gen_rows.py` turns each row into one request line on stdin:

  <fn> TAB <arg> TAB <arg> ...

where each <arg> is `<kind>:<payload>`:

  n:<decimal>        a Nat (positions; `UIntN` arguments via `ofNat`)
  i:<decimal>        an Int, maybe negative (`IntN` arguments via `ofInt`)
  s:<hex>            a String, its UTF-8 bytes in hex
  y:<hex>            a ByteArray, its bytes in hex
  f:<hex>            a Float, `Float.ofBits`; `F:` is its negation
  g:<hex>            a Float32, `Float32.ofBits`; `G:` is its negation
  l:<hex>:<d>:<e>    a String.Slice, `(s.toSlice.drop d).dropEnd e`

For each request it prints every line the evaluation wrote to stderr (a
panic message) as `@panic <line>`, the bits of a `Float`/`Float32` result
(`toBits`, as `0x` and 16 or 8 hex digits) as `@bits <hex>`, then
`=> <repr of the result>`, and flushes. Run with LEAN_BACKTRACE=0.
-/

inductive Arg where
  | nat (n : Nat)
  | int (i : Int)
  | str (s : String)
  | bytes (b : ByteArray)
  | flt (x : Float)
  | f32 (x : Float32)
  | slice (s : String) (d e : Nat)

def hexVal (c : Char) : Option Nat :=
  if '0' ≤ c ∧ c ≤ '9' then some (c.toNat - '0'.toNat)
  else if 'a' ≤ c ∧ c ≤ 'f' then some (c.toNat - 'a'.toNat + 10)
  else if 'A' ≤ c ∧ c ≤ 'F' then some (c.toNat - 'A'.toNat + 10)
  else none

def fail {α : Type} (msg : String) : IO α := throw (IO.userError msg)

def parseHexNat (s : String) : IO Nat :=
  s.toList.foldlM (init := 0) fun acc c =>
    match hexVal c with
    | some v => pure (acc * 16 + v)
    | none => fail s!"bad hex digit in {s}"

def parseHexBytes (s : String) : IO ByteArray := do
  let cs := s.toList.toArray
  if cs.size % 2 != 0 then fail s!"odd hex length: {s}"
  let mut out := ByteArray.empty
  for i in [0:cs.size / 2] do
    match hexVal cs[2 * i]!, hexVal cs[2 * i + 1]! with
    | some h, some l => out := out.push (UInt8.ofNat (h * 16 + l))
    | _, _ => fail s!"bad hex: {s}"
  return out

def parseNat (s : String) : IO Nat :=
  match s.toNat? with
  | some n => pure n
  | none => fail s!"bad Nat: {s}"

def parseInt (s : String) : IO Int :=
  match s.toInt? with
  | some i => pure i
  | none => fail s!"bad Int: {s}"

def parseStr (hex : String) : IO String := do
  match String.fromUTF8? (← parseHexBytes hex) with
  | some s => pure s
  | none => fail s!"not UTF-8: {hex}"

def parseArg (tok : String) : IO Arg := do
  match tok.splitOn ":" with
  | ["n", p] => return .nat (← parseNat p)
  | ["i", p] => return .int (← parseInt p)
  | ["s", p] => return .str (← parseStr p)
  | ["y", p] => return .bytes (← parseHexBytes p)
  | ["f", p] => return .flt (Float.ofBits (UInt64.ofNat (← parseHexNat p)))
  | ["F", p] => return .flt (-(Float.ofBits (UInt64.ofNat (← parseHexNat p))))
  | ["g", p] => return .f32 (Float32.ofBits (UInt32.ofNat (← parseHexNat p)))
  | ["G", p] => return .f32 (-(Float32.ofBits (UInt32.ofNat (← parseHexNat p))))
  | ["l", p, d, e] => return .slice (← parseStr p) (← parseNat d) (← parseNat e)
  | _ => fail s!"bad argument: {tok}"

def natA : Arg → IO Nat
  | .nat n => pure n
  | _ => fail "expected a Nat"
def intA : Arg → IO Int
  | .int i => pure i
  | .nat n => pure n
  | _ => fail "expected an Int"
def strA : Arg → IO String
  | .str s => pure s
  | _ => fail "expected a String"
def bytesA : Arg → IO ByteArray
  | .bytes b => pure b
  | _ => fail "expected a ByteArray"
def fA : Arg → IO Float
  | .flt x => pure x
  | _ => fail "expected a Float"
def gA : Arg → IO Float32
  | .f32 x => pure x
  | _ => fail "expected a Float32"
def sliceA : Arg → IO String.Slice
  | .slice s d e => pure ((s.toSlice.drop d).dropEnd e)
  | _ => fail "expected a String.Slice"
def posA (a : Arg) : IO String.Pos.Raw := return ⟨← natA a⟩

def u8 (a : Arg) : IO UInt8 := return UInt8.ofNat (← natA a)
def u16 (a : Arg) : IO UInt16 := return UInt16.ofNat (← natA a)
def u32 (a : Arg) : IO UInt32 := return UInt32.ofNat (← natA a)
def u64 (a : Arg) : IO UInt64 := return UInt64.ofNat (← natA a)
def usz (a : Arg) : IO USize := return USize.ofNat (← natA a)
def i8 (a : Arg) : IO Int8 := return Int8.ofInt (← intA a)
def i16 (a : Arg) : IO Int16 := return Int16.ofInt (← intA a)
def i32 (a : Arg) : IO Int32 := return Int32.ofInt (← intA a)
def i64 (a : Arg) : IO Int64 := return Int64.ofInt (← intA a)
def isz (a : Arg) : IO ISize := return ISize.ofInt (← intA a)

def hexDigits (n width : Nat) : String :=
  let ds := Nat.toDigits 16 n
  String.ofList (List.replicate (width - ds.length) '0' ++ ds)

def r {α : Type} [Repr α] (x : α) : String := (repr x).pretty
/-- The bits of the `Float`/`Float32` values in the current result, printed as
`@bits` lines (`repr` shows six decimals only). -/
initialize floatBits : IO.Ref (Array String) ← IO.mkRef #[]

def recordF (x : Float) : IO Unit := floatBits.modify (·.push ("0x" ++ hexDigits x.toBits.toNat 16))
def recordG (x : Float32) : IO Unit := floatBits.modify (·.push ("0x" ++ hexDigits x.toBits.toNat 8))
def retF (x : Float) : IO String := do recordF x; return r x
def retG (x : Float32) : IO String := do recordG x; return r x
def unreachable : String := "!unreachable"

def runFloat (fn : String) (a : List Arg) : IO (Option String) := do
  match fn, a with
  | "Float.toString", [x] => return r (Float.toString (← fA x))
  | "Float.toUInt8", [x] => return r (Float.toUInt8 (← fA x))
  | "Float.toUInt16", [x] => return r (Float.toUInt16 (← fA x))
  | "Float.toUInt32", [x] => return r (Float.toUInt32 (← fA x))
  | "Float.toUInt64", [x] => return r (Float.toUInt64 (← fA x))
  | "Float.toUSize", [x] => return r (Float.toUSize (← fA x))
  | "Float.toInt8", [x] => return r (Float.toInt8 (← fA x))
  | "Float.toInt16", [x] => return r (Float.toInt16 (← fA x))
  | "Float.toInt32", [x] => return r (Float.toInt32 (← fA x))
  | "Float.toInt64", [x] => return r (Float.toInt64 (← fA x))
  | "Float.toISize", [x] => return r (Float.toISize (← fA x))
  | "Float.ofBits", [x] => retF (Float.ofBits (← u64 x))
  | "Float.toBits", [x] => return r (Float.toBits (← fA x))
  | "Float.isNaN", [x] => return r (Float.isNaN (← fA x))
  | "Float.isInf", [x] => return r (Float.isInf (← fA x))
  | "Float.isFinite", [x] => return r (Float.isFinite (← fA x))
  | "Float.frExp", [x] =>
    let (m, e) := Float.frExp (← fA x)
    recordF m
    return r (m, e)
  | "Float.scaleB", [x, i] => retF (Float.scaleB (← fA x) (← intA i))
  | "Float32.toString", [x] => return r (Float32.toString (← gA x))
  | "Float32.toUInt8", [x] => return r (Float32.toUInt8 (← gA x))
  | "Float32.toUInt16", [x] => return r (Float32.toUInt16 (← gA x))
  | "Float32.toUInt32", [x] => return r (Float32.toUInt32 (← gA x))
  | "Float32.toUInt64", [x] => return r (Float32.toUInt64 (← gA x))
  | "Float32.toUSize", [x] => return r (Float32.toUSize (← gA x))
  | "Float32.toInt8", [x] => return r (Float32.toInt8 (← gA x))
  | "Float32.toInt16", [x] => return r (Float32.toInt16 (← gA x))
  | "Float32.toInt32", [x] => return r (Float32.toInt32 (← gA x))
  | "Float32.toInt64", [x] => return r (Float32.toInt64 (← gA x))
  | "Float32.toISize", [x] => return r (Float32.toISize (← gA x))
  | "Float32.ofBits", [x] => retG (Float32.ofBits (← u32 x))
  | "Float32.toBits", [x] => return r (Float32.toBits (← gA x))
  | "Float32.isNaN", [x] => return r (Float32.isNaN (← gA x))
  | "Float32.isInf", [x] => return r (Float32.isInf (← gA x))
  | "Float32.isFinite", [x] => return r (Float32.isFinite (← gA x))
  | "Float32.frExp", [x] =>
    let (m, e) := Float32.frExp (← gA x)
    recordG m
    return r (m, e)
  | "Float32.scaleB", [x, i] => retG (Float32.scaleB (← gA x) (← intA i))
  | _, _ => return none

def runLibm (fn : String) (a : List Arg) : IO (Option String) := do
  match fn, a with
  | "Float.abs", [x] => retF (Float.abs (← fA x))
  | "Float.acos", [x] => retF (Float.acos (← fA x))
  | "Float.acosh", [x] => retF (Float.acosh (← fA x))
  | "Float.asin", [x] => retF (Float.asin (← fA x))
  | "Float.asinh", [x] => retF (Float.asinh (← fA x))
  | "Float.atan", [x] => retF (Float.atan (← fA x))
  | "Float.atan2", [y, x] => retF (Float.atan2 (← fA y) (← fA x))
  | "Float.atanh", [x] => retF (Float.atanh (← fA x))
  | "Float.cbrt", [x] => retF (Float.cbrt (← fA x))
  | "Float.ceil", [x] => retF (Float.ceil (← fA x))
  | "Float.cos", [x] => retF (Float.cos (← fA x))
  | "Float.cosh", [x] => retF (Float.cosh (← fA x))
  | "Float.exp", [x] => retF (Float.exp (← fA x))
  | "Float.exp2", [x] => retF (Float.exp2 (← fA x))
  | "Float.floor", [x] => retF (Float.floor (← fA x))
  | "Float.log", [x] => retF (Float.log (← fA x))
  | "Float.log10", [x] => retF (Float.log10 (← fA x))
  | "Float.log2", [x] => retF (Float.log2 (← fA x))
  | "Float.pow", [x, y] => retF (Float.pow (← fA x) (← fA y))
  | "Float.round", [x] => retF (Float.round (← fA x))
  | "Float.sin", [x] => retF (Float.sin (← fA x))
  | "Float.sinh", [x] => retF (Float.sinh (← fA x))
  | "Float.sqrt", [x] => retF (Float.sqrt (← fA x))
  | "Float.tan", [x] => retF (Float.tan (← fA x))
  | "Float.tanh", [x] => retF (Float.tanh (← fA x))
  | "Float32.abs", [x] => retG (Float32.abs (← gA x))
  | "Float32.acos", [x] => retG (Float32.acos (← gA x))
  | "Float32.acosh", [x] => retG (Float32.acosh (← gA x))
  | "Float32.asin", [x] => retG (Float32.asin (← gA x))
  | "Float32.asinh", [x] => retG (Float32.asinh (← gA x))
  | "Float32.atan", [x] => retG (Float32.atan (← gA x))
  | "Float32.atan2", [y, x] => retG (Float32.atan2 (← gA y) (← gA x))
  | "Float32.atanh", [x] => retG (Float32.atanh (← gA x))
  | "Float32.cbrt", [x] => retG (Float32.cbrt (← gA x))
  | "Float32.ceil", [x] => retG (Float32.ceil (← gA x))
  | "Float32.cos", [x] => retG (Float32.cos (← gA x))
  | "Float32.cosh", [x] => retG (Float32.cosh (← gA x))
  | "Float32.exp", [x] => retG (Float32.exp (← gA x))
  | "Float32.exp2", [x] => retG (Float32.exp2 (← gA x))
  | "Float32.floor", [x] => retG (Float32.floor (← gA x))
  | "Float32.log", [x] => retG (Float32.log (← gA x))
  | "Float32.log10", [x] => retG (Float32.log10 (← gA x))
  | "Float32.log2", [x] => retG (Float32.log2 (← gA x))
  | "Float32.pow", [x, y] => retG (Float32.pow (← gA x) (← gA y))
  | "Float32.round", [x] => retG (Float32.round (← gA x))
  | "Float32.sin", [x] => retG (Float32.sin (← gA x))
  | "Float32.sinh", [x] => retG (Float32.sinh (← gA x))
  | "Float32.sqrt", [x] => retG (Float32.sqrt (← gA x))
  | "Float32.tan", [x] => retG (Float32.tan (← gA x))
  | "Float32.tanh", [x] => retG (Float32.tanh (← gA x))
  | _, _ => return none

def runUInt (fn : String) (a : List Arg) : IO (Option String) := do
  match fn, a with
  | "UInt8.div", [x, y] => return r (UInt8.div (← u8 x) (← u8 y))
  | "UInt8.mod", [x, y] => return r (UInt8.mod (← u8 x) (← u8 y))
  | "UInt8.shiftLeft", [x, y] => return r (UInt8.shiftLeft (← u8 x) (← u8 y))
  | "UInt8.shiftRight", [x, y] => return r (UInt8.shiftRight (← u8 x) (← u8 y))
  | "UInt8.log2", [x] => return r (UInt8.log2 (← u8 x))
  | "UInt8.ofNat", [n] => return r (UInt8.ofNat (← natA n))
  | "UInt8.toNat", [x] => return r (UInt8.toNat (← u8 x))
  | "UInt16.div", [x, y] => return r (UInt16.div (← u16 x) (← u16 y))
  | "UInt16.mod", [x, y] => return r (UInt16.mod (← u16 x) (← u16 y))
  | "UInt16.shiftLeft", [x, y] => return r (UInt16.shiftLeft (← u16 x) (← u16 y))
  | "UInt16.shiftRight", [x, y] => return r (UInt16.shiftRight (← u16 x) (← u16 y))
  | "UInt16.log2", [x] => return r (UInt16.log2 (← u16 x))
  | "UInt16.ofNat", [n] => return r (UInt16.ofNat (← natA n))
  | "UInt16.toNat", [x] => return r (UInt16.toNat (← u16 x))
  | "UInt32.div", [x, y] => return r (UInt32.div (← u32 x) (← u32 y))
  | "UInt32.mod", [x, y] => return r (UInt32.mod (← u32 x) (← u32 y))
  | "UInt32.shiftLeft", [x, y] => return r (UInt32.shiftLeft (← u32 x) (← u32 y))
  | "UInt32.shiftRight", [x, y] => return r (UInt32.shiftRight (← u32 x) (← u32 y))
  | "UInt32.log2", [x] => return r (UInt32.log2 (← u32 x))
  | "UInt32.ofNat", [n] => return r (UInt32.ofNat (← natA n))
  | "UInt32.toNat", [x] => return r (UInt32.toNat (← u32 x))
  | "UInt64.div", [x, y] => return r (UInt64.div (← u64 x) (← u64 y))
  | "UInt64.mod", [x, y] => return r (UInt64.mod (← u64 x) (← u64 y))
  | "UInt64.shiftLeft", [x, y] => return r (UInt64.shiftLeft (← u64 x) (← u64 y))
  | "UInt64.shiftRight", [x, y] => return r (UInt64.shiftRight (← u64 x) (← u64 y))
  | "UInt64.log2", [x] => return r (UInt64.log2 (← u64 x))
  | "UInt64.ofNat", [n] => return r (UInt64.ofNat (← natA n))
  | "UInt64.toNat", [x] => return r (UInt64.toNat (← u64 x))
  | "USize.div", [x, y] => return r (USize.div (← usz x) (← usz y))
  | "USize.mod", [x, y] => return r (USize.mod (← usz x) (← usz y))
  | "USize.shiftLeft", [x, y] => return r (USize.shiftLeft (← usz x) (← usz y))
  | "USize.shiftRight", [x, y] => return r (USize.shiftRight (← usz x) (← usz y))
  | "USize.log2", [x] => return r (USize.log2 (← usz x))
  | "USize.ofNat", [n] => return r (USize.ofNat (← natA n))
  | "USize.toNat", [x] => return r (USize.toNat (← usz x))
  | _, _ => return none

def runSInt (fn : String) (a : List Arg) : IO (Option String) := do
  match fn, a with
  | "Int8.div", [x, y] => return r (Int8.div (← i8 x) (← i8 y))
  | "Int8.mod", [x, y] => return r (Int8.mod (← i8 x) (← i8 y))
  | "Int8.shiftLeft", [x, y] => return r (Int8.shiftLeft (← i8 x) (← i8 y))
  | "Int8.shiftRight", [x, y] => return r (Int8.shiftRight (← i8 x) (← i8 y))
  | "Int8.abs", [x] => return r (Int8.abs (← i8 x))
  | "Int8.decLt", [x, y] => return r (decide ((← i8 x) < (← i8 y)))
  | "Int8.decLe", [x, y] => return r (decide ((← i8 x) ≤ (← i8 y)))
  | "Int8.toInt", [x] => return r (Int8.toInt (← i8 x))
  | "Int8.ofInt", [i] => return r (Int8.ofInt (← intA i))
  | "Int8.ofNat", [n] => return r (Int8.ofNat (← natA n))
  | "Int8.toFloat", [x] => retF (Int8.toFloat (← i8 x))
  | "Int8.toFloat32", [x] => retG (Int8.toFloat32 (← i8 x))
  | "Int8.toInt16", [x] => return r (Int8.toInt16 (← i8 x))
  | "Int8.toInt32", [x] => return r (Int8.toInt32 (← i8 x))
  | "Int8.toInt64", [x] => return r (Int8.toInt64 (← i8 x))
  | "Int8.toISize", [x] => return r (Int8.toISize (← i8 x))
  | "Int16.div", [x, y] => return r (Int16.div (← i16 x) (← i16 y))
  | "Int16.mod", [x, y] => return r (Int16.mod (← i16 x) (← i16 y))
  | "Int16.shiftLeft", [x, y] => return r (Int16.shiftLeft (← i16 x) (← i16 y))
  | "Int16.shiftRight", [x, y] => return r (Int16.shiftRight (← i16 x) (← i16 y))
  | "Int16.abs", [x] => return r (Int16.abs (← i16 x))
  | "Int16.decLt", [x, y] => return r (decide ((← i16 x) < (← i16 y)))
  | "Int16.decLe", [x, y] => return r (decide ((← i16 x) ≤ (← i16 y)))
  | "Int16.toInt", [x] => return r (Int16.toInt (← i16 x))
  | "Int16.ofInt", [i] => return r (Int16.ofInt (← intA i))
  | "Int16.ofNat", [n] => return r (Int16.ofNat (← natA n))
  | "Int16.toFloat", [x] => retF (Int16.toFloat (← i16 x))
  | "Int16.toFloat32", [x] => retG (Int16.toFloat32 (← i16 x))
  | "Int16.toInt8", [x] => return r (Int16.toInt8 (← i16 x))
  | "Int16.toInt32", [x] => return r (Int16.toInt32 (← i16 x))
  | "Int16.toInt64", [x] => return r (Int16.toInt64 (← i16 x))
  | "Int16.toISize", [x] => return r (Int16.toISize (← i16 x))
  | "Int32.div", [x, y] => return r (Int32.div (← i32 x) (← i32 y))
  | "Int32.mod", [x, y] => return r (Int32.mod (← i32 x) (← i32 y))
  | "Int32.shiftLeft", [x, y] => return r (Int32.shiftLeft (← i32 x) (← i32 y))
  | "Int32.shiftRight", [x, y] => return r (Int32.shiftRight (← i32 x) (← i32 y))
  | "Int32.abs", [x] => return r (Int32.abs (← i32 x))
  | "Int32.decLt", [x, y] => return r (decide ((← i32 x) < (← i32 y)))
  | "Int32.decLe", [x, y] => return r (decide ((← i32 x) ≤ (← i32 y)))
  | "Int32.toInt", [x] => return r (Int32.toInt (← i32 x))
  | "Int32.ofInt", [i] => return r (Int32.ofInt (← intA i))
  | "Int32.ofNat", [n] => return r (Int32.ofNat (← natA n))
  | "Int32.toFloat", [x] => retF (Int32.toFloat (← i32 x))
  | "Int32.toFloat32", [x] => retG (Int32.toFloat32 (← i32 x))
  | "Int32.toInt8", [x] => return r (Int32.toInt8 (← i32 x))
  | "Int32.toInt16", [x] => return r (Int32.toInt16 (← i32 x))
  | "Int32.toInt64", [x] => return r (Int32.toInt64 (← i32 x))
  | "Int32.toISize", [x] => return r (Int32.toISize (← i32 x))
  | "Int64.div", [x, y] => return r (Int64.div (← i64 x) (← i64 y))
  | "Int64.mod", [x, y] => return r (Int64.mod (← i64 x) (← i64 y))
  | "Int64.shiftLeft", [x, y] => return r (Int64.shiftLeft (← i64 x) (← i64 y))
  | "Int64.shiftRight", [x, y] => return r (Int64.shiftRight (← i64 x) (← i64 y))
  | "Int64.abs", [x] => return r (Int64.abs (← i64 x))
  | "Int64.decLt", [x, y] => return r (decide ((← i64 x) < (← i64 y)))
  | "Int64.decLe", [x, y] => return r (decide ((← i64 x) ≤ (← i64 y)))
  | "Int64.toInt", [x] => return r (Int64.toInt (← i64 x))
  | "Int64.ofInt", [i] => return r (Int64.ofInt (← intA i))
  | "Int64.ofNat", [n] => return r (Int64.ofNat (← natA n))
  | "Int64.toFloat", [x] => retF (Int64.toFloat (← i64 x))
  | "Int64.toFloat32", [x] => retG (Int64.toFloat32 (← i64 x))
  | "Int64.toInt8", [x] => return r (Int64.toInt8 (← i64 x))
  | "Int64.toInt16", [x] => return r (Int64.toInt16 (← i64 x))
  | "Int64.toInt32", [x] => return r (Int64.toInt32 (← i64 x))
  | "Int64.toISize", [x] => return r (Int64.toISize (← i64 x))
  | "ISize.div", [x, y] => return r (ISize.div (← isz x) (← isz y))
  | "ISize.mod", [x, y] => return r (ISize.mod (← isz x) (← isz y))
  | "ISize.shiftLeft", [x, y] => return r (ISize.shiftLeft (← isz x) (← isz y))
  | "ISize.shiftRight", [x, y] => return r (ISize.shiftRight (← isz x) (← isz y))
  | "ISize.abs", [x] => return r (ISize.abs (← isz x))
  | "ISize.decLt", [x, y] => return r (decide ((← isz x) < (← isz y)))
  | "ISize.decLe", [x, y] => return r (decide ((← isz x) ≤ (← isz y)))
  | "ISize.toInt", [x] => return r (ISize.toInt (← isz x))
  | "ISize.ofInt", [i] => return r (ISize.ofInt (← intA i))
  | "ISize.ofNat", [n] => return r (ISize.ofNat (← natA n))
  | "ISize.toFloat", [x] => retF (ISize.toFloat (← isz x))
  | "ISize.toFloat32", [x] => retG (ISize.toFloat32 (← isz x))
  | "ISize.toInt8", [x] => return r (ISize.toInt8 (← isz x))
  | "ISize.toInt16", [x] => return r (ISize.toInt16 (← isz x))
  | "ISize.toInt32", [x] => return r (ISize.toInt32 (← isz x))
  | "ISize.toInt64", [x] => return r (ISize.toInt64 (← isz x))
  | _, _ => return none

def runString (fn : String) (a : List Arg) : IO (Option String) := do
  match fn, a with
  | "String.hash", [s] => return r (String.hash (← strA s))
  | "ByteArray.hash", [b] => return r (ByteArray.hash (← bytesA b))
  | "String.Slice.hash", [l] => return r (String.Slice.hash (← sliceA l))
  | "mixHash", [x, y] => return r (mixHash (← u64 x) (← u64 y))
  | "String.Pos.Raw.get", [s, p] => return r (String.Pos.Raw.get (← strA s) (← posA p))
  | "String.Pos.Raw.get?", [s, p] => return r (String.Pos.Raw.get? (← strA s) (← posA p))
  | "String.Pos.Raw.get!", [s, p] => return r (String.Pos.Raw.get! (← strA s) (← posA p))
  | "String.Pos.Raw.get'", [s, p] =>
    let s ← strA s
    let p ← posA p
    if h : String.Pos.Raw.atEnd s p then return unreachable
    else return r (String.Pos.Raw.get' s p h)
  | "String.decodeChar", [s, n] =>
    let s ← strA s
    let n ← natA n
    if h : (s.toByteArray.utf8DecodeChar? n).isSome then return r (String.decodeChar s n h)
    else return unreachable
  | "String.Pos.Raw.next", [s, p] => return r (String.Pos.Raw.next (← strA s) (← posA p))
  | "String.Pos.Raw.next'", [s, p] =>
    let s ← strA s
    let p ← posA p
    if h : String.Pos.Raw.atEnd s p then return unreachable
    else return r (String.Pos.Raw.next' s p h)
  | "String.Pos.Raw.prev", [s, p] => return r (String.Pos.Raw.prev (← strA s) (← posA p))
  | "String.Pos.Raw.atEnd", [s, p] => return r (String.Pos.Raw.atEnd (← strA s) (← posA p))
  | "String.Pos.Raw.isValid", [s, p] => return r (String.Pos.Raw.isValid (← strA s) (← posA p))
  | "String.Pos.Raw.extract", [s, b, e] =>
    return r (String.Pos.Raw.extract (← strA s) (← posA b) (← posA e))
  | "String.extract", [s, b, e] =>
    let s ← strA s
    match s.pos? (← posA b), s.pos? (← posA e) with
    | some b, some e => return r (String.extract b e)
    | _, _ => return unreachable
  | "String.getUTF8Byte", [s, p] =>
    let s ← strA s
    let p ← posA p
    if h : p < s.rawEndPos then return r (String.getUTF8Byte s p h)
    else return unreachable
  | "String.Internal.ugetUTF8Byte", [s, n] =>
    let s ← strA s
    let n ← usz n
    if h : n.toNat < s.utf8ByteSize then return r (String.Internal.ugetUTF8Byte s n h)
    else return unreachable
  | "String.length", [s] => return r (String.length (← strA s))
  | "String.Slice.Pattern.Internal.memcmpStr", [s1, s2, l, r', n] =>
    let s1 ← strA s1
    let s2 ← strA s2
    let l ← posA l
    let r' ← posA r'
    let n ← posA n
    if h1 : n.offsetBy l ≤ s1.rawEndPos then
      if h2 : n.offsetBy r' ≤ s2.rawEndPos then
        return r (String.Slice.Pattern.Internal.memcmpStr s1 s2 l r' n h1 h2)
      else return unreachable
    else return unreachable
  | "String.decidableLT", [s1, s2] => return r (decide ((← strA s1) < (← strA s2)))
  | "String.compare", [s1, s2] => return r (String.compare (← strA s1) (← strA s2))
  | "String.Slice.instDecidableLt", [l1, l2] => return r (decide ((← sliceA l1) < (← sliceA l2)))
  | _, _ => return none

def runFn (fn : String) (args : List Arg) : IO String := do
  if let some out ← runFloat fn args then return out
  if let some out ← runLibm fn args then return out
  if let some out ← runUInt fn args then return out
  if let some out ← runSInt fn args then return out
  if let some out ← runString fn args then return out
  fail s!"unknown function or arity: {fn} ({args.length} arguments)"

def runLine (line : String) : IO String := do
  match line.splitOn "\t" with
  | [] => fail "empty request"
  | fn :: toks =>
    let args ← toks.mapM parseArg
    runFn fn args

/-- Evaluates one request with stdout and stderr captured, so that a panic
message is attributed to its row. The line is read back from a ref inside
the captured region, so its evaluation cannot be moved out of it. -/
def evalRow (line : String) : IO Unit := do
  let lineRef ← IO.mkRef line
  let outRef ← IO.mkRef ""
  floatBits.set #[]
  let (captured, ()) ← IO.FS.withIsolatedStreams (isolateStderr := true) do
    let l ← lineRef.get
    try
      let res ← runLine l
      outRef.set res
    catch e =>
      outRef.set s!"!error {e}"
  let stdout ← IO.getStdout
  for errLine in captured.splitOn "\n" do
    if errLine != "" then stdout.putStrLn s!"@panic {errLine}"
  for b in ← floatBits.get do
    stdout.putStrLn s!"@bits {b}"
  stdout.putStrLn s!"=> {← outRef.get}"
  stdout.flush

partial def loop (stdin : IO.FS.Stream) : IO Unit := do
  let line ← stdin.getLine
  if line.isEmpty then return
  let line := line.trimAsciiEnd.copy
  if !line.isEmpty then evalRow line
  loop stdin

def main : IO Unit := do
  loop (← IO.getStdin)
