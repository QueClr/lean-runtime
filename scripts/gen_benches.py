#!/usr/bin/env python3
"""Generate the micro-benchmarks of `semantics` (CONTRIBUTING.md, "Performance").

    scripts/gen_benches.py

writes one Rust binary per public function of `lean_runtime::semantics`
(`benches/rust/src/bin/<name>.rs`) and its native twin, a Lean 4.34.0
program of the same name that makes the same calls through Lean's normal API
(`benches/native/Bench/<Name>.lean`, one `lean_exe` each). Both take one
argument N, run the same loop of N calls on operands drawn from the same LCG
(or on the same input string), print the same checksum on stdout line 1 and
`kernel_ns <ns>` on line 2: the monotonic time around the loop, the input
built before it and the result consumed after it through an opaque sink
(`black_box`, `Bench.pin`). The timing driver is separate; no timing runs
here.
"""

import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
RUST = ROOT / "benches" / "rust"
LEAN = ROOT / "benches" / "native"

# Operands drawn from the LCG state `x`: Rust and Lean spellings of the same value.
OPS = {
    # unsigned operands of each width, and a shift amount 0..255
    "a8": ("(x >> 56) as u8", "(x >>> 56).toUInt8"),
    "b8": ("(x >> 48) as u8", "(x >>> 48).toUInt8"),
    "a16": ("(x >> 48) as u16", "(x >>> 48).toUInt16"),
    "b16": ("(x >> 32) as u16", "(x >>> 32).toUInt16"),
    "a32": ("(x >> 32) as u32", "(x >>> 32).toUInt32"),
    "b32": ("x as u32", "x.toUInt32"),
    "a64": ("x", "x"),
    "b64": ("x.rotate_left(17)", "((x <<< 17) ||| (x >>> 47))"),
    "s8": ("((x >> 8) & 0xff) as u8", "((x >>> 8) &&& 0xff).toUInt8"),
    "s16": ("((x >> 8) & 0xff) as u16", "((x >>> 8) &&& 0xff).toUInt16"),
    "s32": ("((x >> 8) & 0xff) as u32", "((x >>> 8) &&& 0xff).toUInt32"),
    "s64": ("(x >> 8) & 0xff", "((x >>> 8) &&& 0xff)"),
    # a Nat below 2^62 and an Int in the int32 range (scalars in Lean's encoding)
    "nat": ("x >> 2", "(x >>> 2).toNat"),
    "int": ("((x >> 33) as i64 - (1 << 30)) as u64", "(Int.ofNat (x >>> 33).toNat - 1073741824)"),
    # a uniform double in [0, 1)
    "u01": ("((x >> 11) as f64 * (1.0 / 9007199254740992.0))",
            "((x >>> 11).toFloat * 1.1102230246251565e-16)"),
    # an exponent in [-1024, 1023]
    "exp": ("(((x >> 8) & 0x7ff) as i64 - 1024)", "(Int.ofNat ((x >>> 8) &&& 0x7ff).toNat - 1024)"),
}

UINTS = [("uint8", "UInt8", "u8", "8"), ("uint16", "UInt16", "u16", "16"),
         ("uint32", "UInt32", "u32", "32"), ("uint64", "UInt64", "u64", "64"),
         ("usize", "USize", "usize", "64")]
SINTS = [("int8", "Int8", "u8", "8"), ("int16", "Int16", "u16", "16"),
         ("int32", "Int32", "u32", "32"), ("int64", "Int64", "u64", "64"),
         ("isize", "ISize", "usize", "64")]


def op(name, lang):
    return OPS[name][0 if lang == "rust" else 1]


def frange(lo, width, lang, f32=False):
    """A float in [lo, lo + width) from the LCG; lo and width are dyadic, so
    both languages read the literals exactly."""
    if lang == "rust":
        v = f"({lo!r} + {op('u01', 'rust')} * {width!r})"
        return f"({v} as f32)" if f32 else v
    lo = lo if isinstance(lo, str) else lean_lit(lo)
    width = width if isinstance(width, str) else lean_lit(width)
    v = f"({lo} + {op('u01', 'lean')} * {width})"
    return f"({v}).toFloat32" if f32 else v


def lean_lit(v):
    return repr(v).replace("e+", "e")


class Bench:
    """One benchmark: `shape` is the accumulator (`u64`, `f64`, `f32`), `input`
    the prebuilt input, `rust`/`lean` the per-call expression in terms of `x`
    (and the input's names)."""

    def __init__(self, name, fn, rust, lean, shape="u64", input="none", note=""):
        self.name, self.fn, self.rust, self.lean = name, fn, rust, lean
        self.shape, self.input, self.note = shape, input, note


BENCHES = []


def add(*a, **k):
    BENCHES.append(Bench(*a, **k))


# ---------------------------------------------------------------- hash

add("uint64_mix_hash", "hash::uint64_mix_hash", "hash::uint64_mix_hash(acc, x)", "mixHash acc x",
    shape="chain")
add("hash_str", "hash::hash_str", "hash::hash_str(black_box(s.as_bytes()), 11)", "String.hash s",
    input="str", note="the seed-11 call, as `String.hash`")
add("string_hash", "hash::string_hash", "hash::string_hash(black_box(s.as_bytes()))", "String.hash s",
    input="str")
add("byte_array_hash", "hash::byte_array_hash", "hash::byte_array_hash(black_box(b.as_slice()))",
    "ByteArray.hash b", input="bytes")
add("slice_hash", "hash::slice_hash", "hash::slice_hash(black_box(&s.as_bytes()[*a..]))",
    "String.Slice.hash sl", input="slice")

# ---------------------------------------------------------------- float, float32

CONV_RANGES = {"8": (-300.0, 600.0), "16": (-70000.0, 140000.0), "32": (-5e9, 1e10),
               "64": (-2e19, 4e19)}
for mod, lean_ty, f32 in [("float", "Float", False), ("float32", "Float32", True)]:
    for ty, lean_t, rt, w in UINTS:
        lo, wd = CONV_RANGES[w]
        name = ty.replace("uint", "uint")
        add(f"{mod}_to_{ty}", f"{mod}::to_{ty}",
            f"{mod}::to_{ty}({frange(lo, wd, 'rust', f32)}) as u64",
            f"({lean_ty}.to{lean_t} {frange(lean_lit(lo), lean_lit(wd), 'lean', f32)}).toUInt64")
    for ty, lean_t, rt, w in SINTS:
        lo, wd = CONV_RANGES[w]
        add(f"{mod}_to_{ty}", f"{mod}::to_{ty}",
            f"{mod}::to_{ty}({frange(lo, wd, 'rust', f32)}) as u64",
            f"({lean_ty}.to{lean_t} {frange(lean_lit(lo), lean_lit(wd), 'lean', f32)}).to{lean_t.replace('Int', 'UInt')}.toUInt64"
            if lean_t != "ISize" else
            f"({lean_ty}.toISize {frange(lean_lit(lo), lean_lit(wd), 'lean', f32)}).toUSize.toUInt64")
    fv = frange(-1e6, 2e6, "rust", f32)
    lv = frange(lean_lit(-1e6), lean_lit(2e6), "lean", f32)
    add(f"{mod}_to_string", f"{mod}::to_string",
        f"{{ let mut t = String::new(); {mod}::to_string({fv}, &mut t).unwrap(); t.len() as u64 }}",
        f"({lean_ty}.toString {lv}).utf8ByteSize.toUInt64",
        note="a fresh String per call, as Lean makes a string per call")
    bits = "x" if not f32 else "x as u32"
    lbits = "x" if not f32 else "x.toUInt32"
    add(f"{mod}_of_bits", f"{mod}::of_bits", f"({mod}::of_bits({bits}) < 1.0) as u64",
        f"(if {lean_ty}.ofBits {lbits} < 1.0 then 1 else 0)")
    add(f"{mod}_to_bits", f"{mod}::to_bits", f"{mod}::to_bits({fv}) as u64",
        f"({lv}).toBits.toUInt64")
    for f in ["isnan", "isfinite", "isinf"]:
        lf = {"isnan": "isNaN", "isfinite": "isFinite", "isinf": "isInf"}[f]
        add(f"{mod}_{f}", f"{mod}::{f}", f"{mod}::{f}({fv}) as u64",
            f"(if {lean_ty}.{lf} {lv} then 1 else 0)")
    cast = "as f64" if not f32 else "as f32"
    add(f"{mod}_frexp", f"{mod}::frexp",
        f"{{ let (m, e) = {mod}::frexp({fv}); m + e {cast} }}",
        f"(match {lean_ty}.frExp {lv} with | (m, e) => m + e.toInt64.to{lean_ty})",
        shape="f32" if f32 else "f64")
    sv = frange(-100.0, 200.0, "rust", f32)
    lsv = frange(lean_lit(-100.0), lean_lit(200.0), "lean", f32)
    add(f"{mod}_scaleb", f"{mod}::scaleb", f"{mod}::scaleb({sv}, {op('exp', 'rust')})",
        f"{lean_ty}.scaleB {lsv} {op('exp', 'lean')}", shape="f32" if f32 else "f64")

# ---------------------------------------------------------------- libm

LIBM = {  # name: (lo, width), dyadic
    "fabs": (-128.0, 256.0), "acos": (-1.0, 2.0), "acosh": (1.0, 128.0), "asin": (-1.0, 2.0),
    "asinh": (-128.0, 256.0), "atan": (-128.0, 256.0), "ceil": (-1048576.0, 2097152.0),
    "cos": (-16.0, 32.0), "cosh": (-16.0, 32.0), "exp": (-64.0, 128.0), "exp2": (-64.0, 128.0),
    "floor": (-1048576.0, 2097152.0), "log": (0.0009765625, 1024.0),
    "log10": (0.0009765625, 1024.0), "log2": (0.0009765625, 1024.0),
    "round": (-1048576.0, 2097152.0), "sin": (-16.0, 32.0), "sinh": (-16.0, 32.0),
    "sqrt": (0.0, 1048576.0), "tan": (-16.0, 32.0), "tanh": (-16.0, 32.0),
    "cbrt": (-1048576.0, 2097152.0), "atanh": (-0.9990234375, 1.998046875),
}
LEAN_LIBM = {"fabs": "abs"}
for f32 in [False, True]:
    lean_ty = "Float32" if f32 else "Float"
    for name, (lo, wd) in LIBM.items():
        cname = name + ("f" if f32 else "")
        add(f"libm_{cname}", f"libm::{cname}", f"libm::{cname}({frange(lo, wd, 'rust', f32)})",
            f"{lean_ty}.{LEAN_LIBM.get(name, name)} {frange(lean_lit(lo), lean_lit(wd), 'lean', f32)}",
            shape="f32" if f32 else "f64")
    a2 = "atan2f" if f32 else "atan2"
    add(f"libm_{a2}", f"libm::{a2}",
        f"libm::{a2}({frange(-16.0, 32.0, 'rust', f32)}, {frange(-16.0, 32.0, 'rust', f32).replace('x >> 11', 'x.rotate_left(17) >> 11')})",
        f"{lean_ty}.atan2 {frange('-16.0', '32.0', 'lean', f32)} {frange('-16.0', '32.0', 'lean', f32).replace('x >>> 11', '((x <<< 17) ||| (x >>> 47)) >>> 11')}",
        shape="f32" if f32 else "f64")
    pw = "powf" if f32 else "pow"
    add(f"libm_{pw}", f"libm::{pw}",
        f"libm::{pw}({frange(0.0078125, 8.0, 'rust', f32)}, {frange(-16.0, 32.0, 'rust', f32).replace('x >> 11', 'x.rotate_left(17) >> 11')})",
        f"{lean_ty}.pow {frange('0.0078125', '8.0', 'lean', f32)} {frange('-16.0', '32.0', 'lean', f32).replace('x >>> 11', '((x <<< 17) ||| (x >>> 47)) >>> 11')}",
        shape="f32" if f32 else "f64")

# ---------------------------------------------------------------- uint

for ty, lean_t, rt, w in UINTS:
    a, b, s = (f"a{w}", f"b{w}", f"s{w}")
    for f, lf, args in [("div", "div", (a, b)), ("mod", "mod", (a, b)),
                        ("shift_left", "shiftLeft", (a, s)), ("shift_right", "shiftRight", (a, s))]:
        ra = ", ".join(f"({op(x, 'rust')}) as {rt}" if rt == "usize" else op(x, "rust") for x in args)
        la = " ".join(f"({op(x, 'lean')}).toUSize" if rt == "usize" else op(x, "lean") for x in args)
        add(f"{ty}_{f}", f"uint::{ty}_{f}", f"uint::{ty}_{f}({ra}) as u64",
            f"({lean_t}.{lf} {la}).toUInt64")
    ra = f"({op(a, 'rust')}) as {rt}" if rt == "usize" else op(a, "rust")
    la = f"({op(a, 'lean')}).toUSize" if rt == "usize" else op(a, "lean")
    add(f"{ty}_log2", f"uint::{ty}_log2", f"uint::{ty}_log2({ra}) as u64", f"({lean_t}.log2 {la}).toUInt64")
    add(f"{ty}_of_nat", f"uint::{ty}_of_nat", f"uint::{ty}_of_nat({op('nat', 'rust')}) as u64",
        f"({lean_t}.ofNat {op('nat', 'lean')}).toUInt64")
    add(f"{ty}_to_nat", f"uint::{ty}_to_nat", f"uint::{ty}_to_nat({ra})",
        f"({lean_t}.toNat {la}).toUInt64")

# ---------------------------------------------------------------- sint

SIGNED_OF = {"Int8": "UInt8", "Int16": "UInt16", "Int32": "UInt32", "Int64": "UInt64", "ISize": "USize"}
for ty, lean_t, rt, w in SINTS:
    ut = SIGNED_OF[lean_t]

    def lsigned(name):
        if lean_t == "ISize":
            return f"({op(name, 'lean')}).toUSize.toISize"
        return f"({op(name, 'lean')}).to{lean_t}"

    def rarg(name):
        return f"({op(name, 'rust')}) as usize" if rt == "usize" else op(name, "rust")

    def back(e):
        return f"({e}).to{ut}.toUInt64" if lean_t != "ISize" else f"({e}).toUSize.toUInt64"

    a, b, s = (f"a{w}", f"b{w}", f"s{w}")
    for f, lf, args in [("div", "div", (a, b)), ("mod", "mod", (a, b)),
                        ("shift_left", "shiftLeft", (a, s)), ("shift_right", "shiftRight", (a, s))]:
        add(f"{ty}_{f}", f"sint::{ty}_{f}",
            f"sint::{ty}_{f}({', '.join(rarg(x) for x in args)}) as u64",
            back(f"{lean_t}.{lf} {' '.join(lsigned(x) for x in args)}"))
    add(f"{ty}_abs", f"sint::{ty}_abs", f"sint::{ty}_abs({rarg(a)}) as u64", back(f"{lean_t}.abs {lsigned(a)}"))
    add(f"{ty}_dec_lt", f"sint::{ty}_dec_lt", f"sint::{ty}_dec_lt({rarg(a)}, {rarg(b)}) as u64",
        f"(if {lsigned(a)} < {lsigned(b)} then 1 else 0)")
    add(f"{ty}_dec_le", f"sint::{ty}_dec_le", f"sint::{ty}_dec_le({rarg(a)}, {rarg(b)}) as u64",
        f"(if {lsigned(a)} ≤ {lsigned(b)} then 1 else 0)")
    to_int = "int64_to_int_sint" if ty == "int64" else f"{ty}_to_int"
    add(f"{to_int}", f"sint::{to_int}", f"sint::{to_int}({rarg(a)}) as u64",
        f"({lean_t}.toInt {lsigned(a)}).toInt64.toUInt64")
    add(f"{ty}_of_int", f"sint::{ty}_of_int", f"sint::{ty}_of_int({op('int', 'rust')}) as u64",
        back(f"{lean_t}.ofInt {op('int', 'lean')}"))
    add(f"{ty}_of_nat", f"sint::{ty}_of_nat", f"sint::{ty}_of_nat({op('nat', 'rust')}) as u64",
        back(f"{lean_t}.ofNat {op('nat', 'lean')}"))
    add(f"{ty}_to_float", f"sint::{ty}_to_float", f"sint::{ty}_to_float({rarg(a)})",
        f"{lean_t}.toFloat {lsigned(a)}", shape="f64")
    add(f"{ty}_to_float32", f"sint::{ty}_to_float32", f"sint::{ty}_to_float32({rarg(a)})",
        f"{lean_t}.toFloat32 {lsigned(a)}", shape="f32")
    for ty2, lean_t2, rt2, w2 in SINTS:
        if ty2 == ty:
            continue
        add(f"{ty}_to_{ty2}", f"sint::{ty}_to_{ty2}", f"sint::{ty}_to_{ty2}({rarg(a)}) as u64",
            (f"({lean_t}.to{lean_t2} {lsigned(a)}).to{SIGNED_OF[lean_t2]}.toUInt64" if lean_t2 != "ISize"
             else f"({lean_t}.toISize {lsigned(a)}).toUSize.toUInt64"))

# ---------------------------------------------------------------- string

POS = ("(x % m)", "(x % m).toNat")
add("string_utf8_get", "string::utf8_get", "string::utf8_get(s, x % m) as u64",
    "(String.Pos.Raw.get s ⟨(x % m).toNat⟩).val.toUInt64", input="pos")
add("string_utf8_get_opt", "string::utf8_get_opt",
    "string::utf8_get_opt(s, x % m).map_or(0, u64::from)",
    "(match String.Pos.Raw.get? s ⟨(x % m).toNat⟩ with | some c => c.val.toUInt64 | none => 0)",
    input="pos")
add("string_utf8_get_bang", "string::utf8_get_bang",
    "string::utf8_get_bang(s, ps[(x % np) as usize]).map_or(0, u64::from)",
    "(String.Pos.Raw.get! s ⟨ps[(x % np).toNat]!⟩).val.toUInt64", input="starts",
    note="at character starts only: an invalid position would print a panic message")
add("string_utf8_get_fast", "string::utf8_get_fast", "string::utf8_get_fast(s, x % k) as u64",
    "(let p : String.Pos.Raw := ⟨(x % k).toNat⟩; if h : String.Pos.Raw.atEnd s p then 0 else (String.Pos.Raw.get' s p h).val.toUInt64)",
    input="pos", note="Lean's call carries the proof test `atEnd`")
add("string_utf8_next", "string::utf8_next", "string::utf8_next(s, x % m)",
    "(String.Pos.Raw.next s ⟨(x % m).toNat⟩).byteIdx.toUInt64", input="pos")
add("string_utf8_next_fast", "string::utf8_next_fast", "string::utf8_next_fast(s, x % k)",
    "(let p : String.Pos.Raw := ⟨(x % k).toNat⟩; if h : String.Pos.Raw.atEnd s p then 0 else (String.Pos.Raw.next' s p h).byteIdx.toUInt64)",
    input="pos", note="Lean's call carries the proof test `atEnd`")
add("string_utf8_prev", "string::utf8_prev", "string::utf8_prev(s, x % m)",
    "(String.Pos.Raw.prev s ⟨(x % m).toNat⟩).byteIdx.toUInt64", input="pos")
add("string_utf8_at_end", "string::utf8_at_end", "string::utf8_at_end(s, x % m) as u64",
    "(if String.Pos.Raw.atEnd s ⟨(x % m).toNat⟩ then 1 else 0)", input="pos")
add("string_is_valid_pos", "string::is_valid_pos", "string::is_valid_pos(s, x % m) as u64",
    "(if String.Pos.Raw.isValid s ⟨(x % m).toNat⟩ then 1 else 0)", input="pos")
add("string_utf8_extract", "string::utf8_extract",
    "s[string::utf8_extract(s, x % m, (x >> 32) % m)].to_vec().len() as u64",
    "(String.Pos.Raw.extract s ⟨(x % m).toNat⟩ ⟨((x >>> 32) % m).toNat⟩).utf8ByteSize.toUInt64",
    input="pos", note="the bytes copied into a new buffer, as Lean makes a new string")
add("string_utf8_extract_fast", "string::utf8_extract_fast",
    "s[string::utf8_extract_fast(s, ps[(x % np) as usize], ps[((x >> 32) % np) as usize])].to_vec().len() as u64",
    "(String.extract (Bench.pick s ps (x % np).toNat) (Bench.pick s ps ((x >>> 32) % np).toNat)).utf8ByteSize.toUInt64",
    input="valid", note="the bytes copied into a new buffer, as Lean makes a new string")
add("string_get_byte_fast", "string::get_byte_fast", "string::get_byte_fast(s, x % k) as u64",
    "(let p : String.Pos.Raw := ⟨(x % k).toNat⟩; if h : p < s.rawEndPos then (String.getUTF8Byte s p h).toUInt64 else 0)",
    input="pos", note="Lean's call carries the proof test")
add("string_length", "string::length", "string::length(black_box(s))", "s.length.toUInt64",
    input="pos",
    note="NOT comparable: Lean reads the count cached in the string; this counts the bytes, the "
         "count a translator caches when it makes a string")
add("string_memcmp", "string::memcmp", "string::memcmp(s, t, x % 64, x % 64, 512) as u64",
    "(let l : String.Pos.Raw := ⟨(x % 64).toNat⟩; let n : String.Pos.Raw := ⟨512⟩; if h1 : n.offsetBy l ≤ s.rawEndPos then if h2 : n.offsetBy l ≤ t.rawEndPos then (if String.Slice.Pattern.Internal.memcmpStr s t l l n h1 h2 then 1 else 0) else 0 else 0)",
    input="two", note="Lean's call carries the proof tests")
add("string_lt", "string::lt", "string::lt(black_box(s), t) as u64", "(if decide (s < t) then 1 else 0)",
    input="two")
add("string_compare", "string::compare",
    "(string::compare(black_box(s), t) as i8 + 1) as u64",
    "(match compare s t with | .lt => 0 | .eq => 1 | .gt => 2)", input="two")


# ---------------------------------------------------------------- writers


def camel(name):
    return "".join(p[:1].upper() + p[1:] for p in name.split("_"))


RUST_INPUT = {
    "none": ("()", "()", ""),
    "str": ("String", "mixed()", "let s = inp;"),
    "bytes": ("Vec<u8>", "mixed().into_bytes()", "let b = inp;"),
    "slice": ("(String, usize)", "{ let s = mixed(); let a = s.char_indices().nth(3).unwrap().0; (s, a) }",
              "let (s, a) = (&inp.0, &inp.1);"),
    "pos": ("String", "mixed()",
            "let s = inp.as_bytes(); let k = s.len() as u64; let m = k + 2;"),
    "two": ("(String, String)", "{ let s = mixed(); let mut t = s.clone(); t.pop(); t.push('4'); (s, t) }",
            "let (s, t) = (inp.0.as_bytes(), inp.1.as_bytes());"),
    "starts": ("(String, Vec<u64>)",
               "{ let s = mixed(); let ps = s.char_indices().map(|(i, _)| i as u64).collect(); (s, ps) }",
               "let (s, ps) = (inp.0.as_bytes(), &inp.1); let np = ps.len() as u64;"),
    "valid": ("(String, Vec<u64>)",
              "{ let s = mixed(); let mut ps: Vec<u64> = s.char_indices().map(|(i, _)| i as u64).collect(); ps.push(s.len() as u64); (s, ps) }",
              "let (s, ps) = (inp.0.as_bytes(), &inp.1); let np = ps.len() as u64;"),
}

LEAN_INPUT = {  # type, build, loop binders, kernel bindings, the names passed to the loop
    "none": ("Unit", "pure ()", "", "", ""),
    "str": ("String", "pure Bench.mixed", "(s : String) ", "let s := inp; ", "s "),
    "bytes": ("ByteArray", "pure Bench.mixed.toUTF8", "(b : ByteArray) ", "let b := inp; ", "b "),
    "slice": ("String.Slice", "pure (Bench.mixed.toSlice.drop 3)", "(sl : String.Slice) ",
              "let sl := inp; ", "sl "),
    "pos": ("String", "pure Bench.mixed", "(s : String) (k m : UInt64) ",
            "let s := inp; let k := s.utf8ByteSize.toUInt64; let m := k + 2; ", "s k m "),
    "two": ("String × String", "pure (Bench.mixed, (Bench.mixed.dropEnd 1).copy.push '4')",
            "(s t : String) ", "let s := inp.1; let t := inp.2; ", "s t "),
    "starts": ("String × Array Nat", "pure (Bench.mixed, Bench.starts Bench.mixed)",
               "(s : String) (ps : Array Nat) (np : UInt64) ",
               "let s := inp.1; let ps := inp.2; let np := ps.size.toUInt64; ", "s ps np "),
    "valid": ("Bench.ValidPositions", "pure (Bench.validPositions Bench.mixed)",
              "(s : String) (ps : Array s.Pos) (np : UInt64) ",
              "let s := inp.s; let ps := inp.ps; let np := ps.size.toUInt64; ", "s ps np "),
}


def rust_file(b):
    ity, build, bind = RUST_INPUT[b.input]
    mod = b.fn.split("::")[0]
    note = f"\n//! {b.note}" if b.note else ""
    if b.shape == "chain":
        init, upd, out = "0u64", f"acc = {b.rust};", "acc"
    elif b.shape == "u64":
        init, upd, out = "0u64", f"acc = acc.wrapping_add({b.rust});", "acc"
    elif b.shape == "f64":
        init, upd, out = "0.0f64", f"acc += {b.rust};", "float::to_bits(acc)"
    else:
        init, upd, out = "0.0f32", f"acc += {b.rust};", "float32::to_bits(acc)"
    uses = sorted({mod} | ({"float"} if b.shape == "f64" else set()) | ({"float32"} if b.shape == "f32" else set()))
    needs_bb = "black_box" in b.rust
    return f"""//! Generated by `scripts/gen_benches.py`: times `lean_runtime::semantics::{b.fn}`; the
//! native twin is `benches/native/Bench/{camel(b.name)}.lean`.{note}

#![allow(unused_parens, clippy::double_parens, clippy::unnecessary_cast, clippy::redundant_closure)]

use lean_runtime::semantics::{{{", ".join(uses)}}};
use lean_runtime_bench::{{{"mixed, " if b.input != "none" else ""}run, step, SEED}};
{"use std::hint::black_box;" if needs_bb else ""}
#[allow(unused_variables)]
fn kernel(inp: &{ity}, n: u64) -> u64 {{
    {bind}
    let (mut x, mut acc) = (SEED, {init});
    for _ in 0..n {{
        x = step(x);
        {upd}
    }}
    {out} as u64
}}

fn main() {{
    run(|| {build}, kernel)
}}
"""


def lean_file(b):
    ity, build, binders, bindings, names = LEAN_INPUT[b.input]
    ns = f"Bench.{camel(b.name)}"
    note = f"\n{b.note}." if b.note else ""
    if b.shape == "chain":
        acc_t, init, upd, out = "UInt64", "0", b.lean, "out"
    elif b.shape == "u64":
        acc_t, init, upd, out = "UInt64", "0", f"acc + {b.lean}", "out"
    elif b.shape == "f64":
        acc_t, init, upd, out = "Float", "0.0", f"acc + {b.lean}", "out.toBits"
    else:
        acc_t, init, upd, out = "Float32", "0.0", f"acc + {b.lean}", "out.toBits"
    return f"""/-
Generated by `scripts/gen_benches.py`: the native twin of `benches/rust/src/bin/{b.name}.rs`,
timing what `lean_runtime::semantics::{b.fn}` mirrors, through Lean's API.{note}
-/
import Bench.Harness

namespace {ns}

partial def loop {binders}(n i x : UInt64) (acc : {acc_t}) : {acc_t} :=
  if i < n then
    let x := Bench.step x
    loop {names}n (i + 1) x ({upd})
  else acc

def kernel ({"_inp" if b.input == "none" else "inp"} : {ity}) (n : UInt64) : {acc_t} :=
  {bindings}loop {names}n 0 Bench.SEED {init}

end {ns}

def main (args : List String) : IO UInt32 :=
  Bench.run (fun _ => {build}) {ns}.kernel (fun out => toString ({out})) args
"""


def main():
    names = [b.name for b in BENCHES]
    assert len(names) == len(set(names)), "duplicate bench names"
    bins = RUST / "src" / "bin"
    bins.mkdir(parents=True, exist_ok=True)
    for f in bins.glob("*.rs"):
        f.unlink()
    mods = LEAN / "Bench"
    mods.mkdir(parents=True, exist_ok=True)
    for f in mods.glob("*.lean"):
        if f.name != "Harness.lean":
            f.unlink()
    for b in BENCHES:
        (bins / f"{b.name}.rs").write_text(rust_file(b))
        (mods / f"{camel(b.name)}.lean").write_text(lean_file(b))
    exes = "".join(f'\n[[lean_exe]]\nname = "{b.name}"\nroot = "Bench.{camel(b.name)}"\n'
                   for b in BENCHES)
    (LEAN / "lakefile.toml").write_text(
        "# Generated by scripts/gen_benches.py: one executable per benchmark.\n"
        'name = "bench"\ndefaultTargets = [' + ", ".join(f'"{b.name}"' for b in BENCHES) + ']\n\n[[lean_lib]]\nname = "Bench"\nroots = ["Bench.Harness"]\n' + exes)
    subprocess.run(["rustfmt", "--edition", "2021"] + [str(bins / f"{b.name}.rs") for b in BENCHES],
                   check=True)
    print(f"{len(BENCHES)} benchmarks")


if __name__ == "__main__":
    main()
