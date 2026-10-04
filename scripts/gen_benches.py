#!/usr/bin/env python3
"""Generate the micro-benchmarks of `semantics` (benches/README.md).

    scripts/gen_benches.py

writes one Rust binary per benchmark (`benches/rust/src/bin/<name>.rs`), its native twin, a Lean
4.34.0 program of the same name that makes the same calls through Lean's API
(`benches/native/Bench/<Name>.lean`, one `lean_exe` each, listed in `benches/native/lakefile.toml`),
and the manifest `benches/benches.toml` (each benchmark's function, and whether the pair is
comparable). The shared inputs, glue and checksum are in `benches/rust/src/lib.rs` and
`benches/native/Bench/Harness.lean`, written by hand.

A benchmark runs N iterations of one call on operands read at run time (from the prebuilt arrays
of its input, or from the LCG for the integer operations); Lean's float literals never appear in a
loop (`scripts/check_benches.sh` fails if one does). Each iteration's result enters the checksum
through `mix`, which both sides compute alike.
"""

import pathlib
import struct
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
RUST = ROOT / "benches" / "rust"
LEAN = ROOT / "benches" / "native"


def bits(v):
    return "0x%016X" % struct.unpack("<Q", struct.pack("<d", v))[0]


# ---------------------------------------------------------------- inputs: Rust and Lean twins
# an input is (Rust type, Rust build, Rust prelude, Lean type, Lean build)


def f64_input(lo, width, specials):
    sp = "true" if specials else "false"
    return ("Vec<f64>", f"f64_operands({bits(lo)}, {bits(width)}, {sp})", "let xs = &inp[..];",
            "FloatArray", f"pure (f64Operands {bits(lo)} {bits(width)} {sp})")


def pair_input(lo, width, lo2, width2):
    return ("(Vec<f64>, Vec<f64>)",
            f"f64_pairs({bits(lo)}, {bits(width)}, {bits(lo2)}, {bits(width2)})",
            "let (xs, ys) = (&inp.0[..], &inp.1[..]);",
            "FloatArray × FloatArray",
            f"pure (f64Pairs {bits(lo)} {bits(width)} {bits(lo2)} {bits(width2)})")


POS_KINDS = {"any": ("Any", 0), "inside": ("Inside", 1), "starts": ("Starts", 2)}


def pos_input(kind):
    rk, lk = POS_KINDS[kind]
    return ("PosInput", f"pos_input(mixed(), Positions::{rk})",
            "let (s, ps) = (inp.s.as_bytes(), &inp.ps[..]);",
            "PosInput", f"pure (posInput mixed {lk})")


NONE = ("()", "()", "", "Unit", "pure ()")
STR = ("String", "mixed()", "let s = inp.as_bytes();", "String", "pure mixed")
BYTES = ("Vec<u8>", "mixed().into_bytes()", "let b = &inp[..];", "ByteArray", "pure mixed.toUTF8")
SLICE = ("(String, usize)", "{ let s = mixed(); let a = s.char_indices().nth(3).unwrap().0; (s, a) }",
         "let sl = &inp.0.as_bytes()[inp.1..];", "String.Slice", "pure (mixed.toSlice.drop 3)")
STRS = ("Vec<String>", "variants()", "let ss = &inp[..];", "Array String", "pure variants")
VALID = ("PosInput", "pos_input(mixed(), Positions::Valid)",
         "let (s, ps) = (inp.s.as_bytes(), &inp.ps[..]);", "ValidInput", "pure (validInput mixed)")
WALK = {"ascii": ("String", "ascii()", "let s = inp.as_bytes();", "String", "pure ascii"),
        "mixed": ("String", "mixed()", "let s = inp.as_bytes();", "String", "pure mixed")}

# operand spellings
RF = "get_f64(xs, x >> 52)"                       # the f64 operand
LF = "(inp.get! (x >>> 52).toNat)"
RF2 = ("get_f64(xs, x >> 52)", "get_f64(ys, x >> 52)")
LF2 = ("(inp.1.get! (x >>> 52).toNat)", "(inp.2.get! (x >>> 52).toNat)")
RP = "nat_unbox(get_word(ps, x >> 52))"            # a position (a Nat word, unboxed)
RQ = "nat_unbox(get_word(ps, (x >> 40) & 4095))"
LP = "⟨inp.ps[(x >>> 52).toNat]!⟩"
LQ = "⟨inp.ps[((x >>> 40) &&& 4095).toNat]!⟩"

# LCG operands of the integer benchmarks: (Rust, Lean)
OPS = {
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
    "s64": ("((x >> 8) & 0xff)", "((x >>> 8) &&& 0xff)"),
}


class Bench:
    def __init__(self, name, fn, inp, rust, lean, comparable=True, reason="", note="",
                 shape="mix", walk=None, o12="", state=None):
        self.name, self.fn, self.inp, self.rust, self.lean = name, fn, inp, rust, lean
        self.comparable, self.reason, self.note = comparable, reason, note
        self.shape, self.walk, self.o12 = shape, walk, o12
        # state: (Rust init, Rust update statements, Lean type, Lean init, Lean update): a value
        # carried through the loop and updated in place on both sides (an array the call
        # modifies); `rust`/`lean` then observe it after each update
        self.state = state


O12_LESS = ("by inspection: the crate does strictly less work than C here (Lean allocates or "
            "reads a cached count), so the speed floor holds")


BENCHES = []


def add(*a, **k):
    BENCHES.append(Bench(*a, **k))


# ---------------------------------------------------------------- hash

add("uint64_mix_hash", "hash::uint64_mix_hash", NONE, "hash::uint64_mix_hash(acc, x)",
    "mixHash acc x", shape="chain")
add("hash_str", "hash::hash_str", STR, "hash::hash_str(black_box(s), 11)", "String.hash inp",
    note="the seed-11 call; the Rust operand passes through black_box so the hash of the fixed "
         "string is not hoisted out of the loop")
add("string_hash", "hash::string_hash", STR, "hash::string_hash(black_box(s))", "String.hash inp",
    note="the operand passes through black_box on the Rust side (no hoisting)")
add("byte_array_hash", "hash::byte_array_hash", BYTES, "hash::byte_array_hash(black_box(b))",
    "ByteArray.hash inp", note="the operand passes through black_box on the Rust side")
add("slice_hash", "hash::slice_hash", SLICE, "hash::slice_hash(black_box(sl))",
    "String.Slice.hash inp", note="the operand passes through black_box on the Rust side")

# ---------------------------------------------------------------- float, float32

UINTS = [("uint8", "UInt8", "u8", 8), ("uint16", "UInt16", "u16", 16),
         ("uint32", "UInt32", "u32", 32), ("uint64", "UInt64", "u64", 64),
         ("usize", "USize", "usize", 64)]
SINTS = [("int8", "Int8", "u8", 8), ("int16", "Int16", "u16", 16),
         ("int32", "Int32", "u32", 32), ("int64", "Int64", "u64", 64),
         ("isize", "ISize", "usize", 64)]
UOF = {"Int8": "UInt8", "Int16": "UInt16", "Int32": "UInt32", "Int64": "UInt64", "ISize": "USize"}
CONV = {8: (-300.0, 600.0), 16: (-70000.0, 140000.0), 32: (-5e9, 1e10), 64: (-2e19, 4e19)}


def lean_u64(e, lean_t):
    """A Lean value of a fixed-width type as its unsigned encoding widened to UInt64."""
    if lean_t == "ISize":
        return f"({e}).toUSize.toUInt64"
    if lean_t in UOF:
        return f"({e}).to{UOF[lean_t]}.toUInt64"
    return f"({e}).toUInt64"


for mod, lt, f32 in [("float", "Float", False), ("float32", "Float32", True)]:
    rv = f"({RF} as f32)" if f32 else RF
    lv = f"{LF}.toFloat32" if f32 else LF
    rbits, lbits = ("gbits", ".toBits.toUInt64") if f32 else ("fbits", ".toBits")
    for ty, lean_t, rt, w in UINTS + SINTS:
        lo, wd = CONV[w]
        add(f"{mod}_to_{ty}", f"{mod}::to_{ty}", f64_input(lo, wd, True),
            f"{mod}::to_{ty}({rv}) as u64", lean_u64(f"{lt}.to{lean_t} {lv}", lean_t))
    add(f"{mod}_to_string", f"{mod}::to_string", f64_input(-1e6, 2e6, True),
        f"{{ let mut b = StackBuf::new(); let _ = {mod}::to_string({rv}, &mut b); "
        f"make_ascii_string(b.as_bytes()).byte_size() }}",
        f"({lt}.toString {lv}).utf8ByteSize.toUInt64",
        note="the text formatted into a stack buffer, then one new string object, as "
             "lean_mk_ascii_string_unchecked makes")
    if f32:
        rb = "{ let b = (x >> 32) as u32; if x & 7 == 0 { b | 0x7F80_0000 } else { b } }"
        lb = "(if x &&& 7 == 0 then (x >>> 32).toUInt32 ||| 0x7F800000 else (x >>> 32).toUInt32)"
    else:
        rb = "{ if x & 7 == 0 { x | 0x7FF0_0000_0000_0000 } else { x } }"
        lb = "(if x &&& 7 == 0 then x ||| 0x7FF0000000000000 else x)"
    add(f"{mod}_of_bits", f"{mod}::of_bits", NONE, f"{rbits}({mod}::of_bits({rb}))",
        f"({lt}.ofBits {lb}){lbits}", note="one operand in eight has all exponent bits set")
    add(f"{mod}_to_bits", f"{mod}::to_bits", f64_input(-1e6, 2e6, True),
        f"{mod}::to_bits({rv}) as u64", f"{lv}{lbits}")
    for f, lf in [("isnan", "isNaN"), ("isfinite", "isFinite"), ("isinf", "isInf")]:
        add(f"{mod}_{f}", f"{mod}::{f}", f64_input(-1e6, 2e6, True), f"{mod}::{f}({rv}) as u64",
            f"(if {lt}.{lf} {lv} then 1 else 0)", note="one operand in eight is a special value")
    add(f"{mod}_frexp", f"{mod}::frexp", f64_input(-1e6, 2e6, True),
        f"{{ let (m, e) = {mod}::frexp({rv}); {rbits}(m) ^ (e as i64 as u64) }}",
        f"(match {lt}.frExp {lv} with | (m, e) => m{lbits} ^^^ e.toInt64.toUInt64)",
        comparable=False,
        reason="Lean's API returns a boxed pair holding a boxed float: two allocations per call; "
               "the crate returns registers, and a translator's cost depends on its Prod "
               "representation", o12=O12_LESS)
    add(f"{mod}_scaleb", f"{mod}::scaleb", f64_input(-100.0, 200.0, False),
        f"{rbits}({mod}::scaleb({rv}, int_unbox(int_box(((x >> 8) & 0xfff) as i64 - 2048))))",
        f"({lt}.scaleB {lv} (Int.ofNat ((x >>> 8) &&& 0xfff).toNat - 2048)){lbits}",
        note="the exponent is an Int word on both sides, in [-2048, 2047]")

# ---------------------------------------------------------------- libm

LIBM = {  # name: (lo, width)
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
    lt = "Float32" if f32 else "Float"
    rbits, lbits = ("gbits", ".toBits.toUInt64") if f32 else ("fbits", ".toBits")
    rv = f"({RF} as f32)" if f32 else RF
    lv = f"{LF}.toFloat32" if f32 else LF
    for name, (lo, wd) in LIBM.items():
        cname = name + ("f" if f32 else "")
        add(f"libm_{cname}", f"libm::{cname}", f64_input(lo, wd, False),
            f"{rbits}(libm::{cname}({rv}))", f"({lt}.{LEAN_LIBM.get(name, name)} {lv}){lbits}")
    rx, ry = (f"({RF2[0]} as f32)", f"({RF2[1]} as f32)") if f32 else RF2
    lx, ly = (f"{LF2[0]}.toFloat32", f"{LF2[1]}.toFloat32") if f32 else LF2
    a2, pw = ("atan2f", "powf") if f32 else ("atan2", "pow")
    add(f"libm_{a2}", f"libm::{a2}", pair_input(-16.0, 32.0, -16.0, 32.0),
        f"{rbits}(libm::{a2}({rx}, {ry}))", f"({lt}.atan2 {lx} {ly}){lbits}")
    add(f"libm_{pw}", f"libm::{pw}", pair_input(0.0078125, 8.0, -16.0, 32.0),
        f"{rbits}(libm::{pw}({rx}, {ry}))", f"({lt}.pow {lx} {ly}){lbits}")

# ---------------------------------------------------------------- uint


def rop(name, rt):
    r = OPS[name][0]
    return f"({r}) as usize" if rt == "usize" else r


def lop(name, rt):
    l = OPS[name][1]
    return f"({l}).toUSize" if rt == "usize" else l


for ty, lean_t, rt, w in UINTS:
    a, b, s = f"a{w}", f"b{w}", f"s{w}"
    for f, lf, args in [("div", "div", (a, b)), ("mod", "mod", (a, b)),
                        ("shift_left", "shiftLeft", (a, s)), ("shift_right", "shiftRight", (a, s))]:
        add(f"{ty}_{f}", f"uint::{ty}_{f}",
            NONE, f"uint::{ty}_{f}({', '.join(rop(x, rt) for x in args)}) as u64",
            f"({lean_t}.{lf} {' '.join(lop(x, rt) for x in args)}).toUInt64")
    add(f"{ty}_log2", f"uint::{ty}_log2", NONE, f"uint::{ty}_log2({rop(a, rt)}) as u64",
        f"({lean_t}.log2 {lop(a, rt)}).toUInt64")
    add(f"{ty}_of_nat", f"uint::{ty}_of_nat", NONE,
        f"uint::{ty}_of_nat(nat_unbox(nat_box(x >> 2))) as u64",
        f"({lean_t}.ofNat (x >>> 2).toNat).toUInt64", note="the Nat argument is a Nat word")
    if w == 64:  # a scalar Nat result: below 2^63
        ra = "(x >> 1) as usize" if rt == "usize" else "x >> 1"
        la = "(x >>> 1).toUSize" if rt == "usize" else "(x >>> 1)"
    else:
        ra, la = rop(a, rt), lop(a, rt)
    add(f"{ty}_to_nat", f"uint::{ty}_to_nat", NONE,
        f"nat_unbox(nat_box(uint::{ty}_to_nat({ra})))", f"({lean_t}.toNat {la}).toUInt64",
        note="operands below 2^63, so the Nat is a scalar on both sides (a Nat word)")

# ---------------------------------------------------------------- sint

for ty, lean_t, rt, w in SINTS:
    def rs(name):
        return rop(name, rt)

    def ls(name):
        l = OPS[name][1]
        return f"({l}).toUSize.toISize" if lean_t == "ISize" else f"({l}).to{lean_t}"

    a, b, s = f"a{w}", f"b{w}", f"s{w}"
    for f, lf, args in [("div", "div", (a, b)), ("mod", "mod", (a, b)),
                        ("shift_left", "shiftLeft", (a, s)), ("shift_right", "shiftRight", (a, s))]:
        add(f"{ty}_{f}", f"sint::{ty}_{f}", NONE,
            f"sint::{ty}_{f}({', '.join(rs(x) for x in args)}) as u64",
            lean_u64(f"{lean_t}.{lf} {' '.join(ls(x) for x in args)}", lean_t))
    add(f"{ty}_abs", f"sint::{ty}_abs", NONE, f"sint::{ty}_abs({rs(a)}) as u64",
        lean_u64(f"{lean_t}.abs {ls(a)}", lean_t))
    add(f"{ty}_dec_lt", f"sint::{ty}_dec_lt", NONE, f"sint::{ty}_dec_lt({rs(a)}, {rs(b)}) as u64",
        f"(if {ls(a)} < {ls(b)} then 1 else 0)")
    add(f"{ty}_dec_le", f"sint::{ty}_dec_le", NONE, f"sint::{ty}_dec_le({rs(a)}, {rs(b)}) as u64",
        f"(if {ls(a)} ≤ {ls(b)} then 1 else 0)")
    to_int = "int64_to_int_sint" if ty == "int64" else f"{ty}_to_int"
    if w == 64:  # an Int in the int32 range: a scalar Int word on 64-bit platforms
        ra = "((x >> 32) as u32 as i32 as i64) as u64"
        ra = f"({ra}) as usize" if rt == "usize" else ra
        la = "(x >>> 32).toUInt32.toInt32" + (".toISize" if lean_t == "ISize" else ".toInt64")
    else:
        ra, la = rs(a), ls(a)
    add(to_int, f"sint::{to_int}", NONE, f"int_unbox(int_box(sint::{to_int}({ra}))) as u64",
        f"({lean_t}.toInt {la}).toInt64.toUInt64",
        note="operands in the int32 range, so the Int is a scalar word on both sides")
    add(f"{ty}_of_int", f"sint::{ty}_of_int", NONE,
        f"sint::{ty}_of_int(int_unbox(int_box((x >> 33) as i64 - (1 << 30))) as u64) as u64",
        lean_u64(f"{lean_t}.ofInt (Int.ofNat (x >>> 33).toNat - 1073741824)", lean_t),
        note="the Int argument is an Int word, in the int32 range")
    add(f"{ty}_of_nat", f"sint::{ty}_of_nat", NONE,
        f"sint::{ty}_of_nat(nat_unbox(nat_box(x >> 2))) as u64",
        lean_u64(f"{lean_t}.ofNat (x >>> 2).toNat", lean_t), note="the Nat argument is a Nat word")
    add(f"{ty}_to_float", f"sint::{ty}_to_float", NONE, f"fbits(sint::{ty}_to_float({rs(a)}))",
        f"({lean_t}.toFloat {ls(a)}).toBits")
    add(f"{ty}_to_float32", f"sint::{ty}_to_float32", NONE,
        f"gbits(sint::{ty}_to_float32({rs(a)}))", f"({lean_t}.toFloat32 {ls(a)}).toBits.toUInt64")
    for ty2, lean_t2, rt2, w2 in SINTS:
        if ty2 != ty:
            add(f"{ty}_to_{ty2}", f"sint::{ty}_to_{ty2}", NONE,
                f"sint::{ty}_to_{ty2}({rs(a)}) as u64",
                lean_u64(f"{lean_t}.to{lean_t2} {ls(a)}", lean_t2))

# ---------------------------------------------------------------- string

add("string_utf8_get", "string::utf8_get", pos_input("any"),
    f"u64::from(string::utf8_get(s, {RP}))", f"(String.Pos.Raw.get inp.s {LP}).val.toUInt64")
add("string_utf8_get_opt", "string::utf8_get_opt", pos_input("any"),
    f"match string::utf8_get_opt(s, {RP}) {{ Some(c) => u64::from(c), None => 0x110000 }}",
    f"(match String.Pos.Raw.get? inp.s {LP} with | some c => c.val.toUInt64 | none => 0x110000)",
    comparable=False,
    reason="Lean's API allocates the `some` at every valid position; the crate returns an "
           "Option<u32>, and a translator's cost depends on its Option representation", o12=O12_LESS)
add("string_utf8_get_bang", "string::utf8_get_bang", pos_input("starts"),
    f"u64::from(string::utf8_get_bang(s, {RP}).unwrap_or(string::CHAR_DEFAULT))",
    f"(String.Pos.Raw.get! inp.s {LP}).val.toUInt64",
    note="at character starts only: an invalid position would print a panic message")
add("string_utf8_get_fast", "string::utf8_get_fast", pos_input("inside"),
    f"{{ let p = {RP}; if string::utf8_at_end(s, p) {{ 0 }} else "
    f"{{ u64::from(string::utf8_get_fast(s, p)) }} }}",
    f"(let p : String.Pos.Raw := {LP}; if h : String.Pos.Raw.atEnd inp.s p then 0 "
    f"else (String.Pos.Raw.get' inp.s p h).val.toUInt64)",
    note="both sides make the program's test that supplies the proof (`atEnd`)")
add("string_utf8_next", "string::utf8_next", pos_input("any"),
    f"nat_unbox(nat_box(string::utf8_next(s, {RP})))",
    f"(String.Pos.Raw.next inp.s {LP}).byteIdx.toUInt64")
add("string_utf8_next_fast", "string::utf8_next_fast", pos_input("inside"),
    f"{{ let p = {RP}; if string::utf8_at_end(s, p) {{ 0 }} else "
    f"{{ nat_unbox(nat_box(string::utf8_next_fast(s, p))) }} }}",
    f"(let p : String.Pos.Raw := {LP}; if h : String.Pos.Raw.atEnd inp.s p then 0 "
    f"else (String.Pos.Raw.next' inp.s p h).byteIdx.toUInt64)",
    note="both sides make the program's test that supplies the proof (`atEnd`)")
add("string_utf8_prev", "string::utf8_prev", pos_input("any"),
    f"nat_unbox(nat_box(string::utf8_prev(s, {RP})))",
    f"(String.Pos.Raw.prev inp.s {LP}).byteIdx.toUInt64")
add("string_utf8_at_end", "string::utf8_at_end", pos_input("any"),
    f"string::utf8_at_end(s, {RP}) as u64",
    f"(if String.Pos.Raw.atEnd inp.s {LP} then 1 else 0)")
add("string_is_valid_pos", "string::is_valid_pos", pos_input("any"),
    f"string::is_valid_pos(s, {RP}) as u64",
    f"(if String.Pos.Raw.isValid inp.s {LP} then 1 else 0)")
add("string_utf8_extract", "string::utf8_extract", pos_input("any"),
    f"make_string(&s[string::utf8_extract(s, {RP}, {RQ})]).byte_size()",
    f"(String.Pos.Raw.extract inp.s {LP} {LQ}).utf8ByteSize.toUInt64",
    note="the result is a new string object (allocated even when empty, its characters "
         "counted), as lean_mk_string_from_bytes_unchecked makes")
add("string_utf8_extract_fast", "string::utf8_extract_fast", VALID,
    f"make_string(&s[string::utf8_extract_fast(s, {RP}, {RQ})]).byte_size()",
    "(String.extract (pick inp.s inp.ps (x >>> 52).toNat) "
    "(pick inp.s inp.ps ((x >>> 40) &&& 4095).toNat)).utf8ByteSize.toUInt64",
    note="the result is a new string object, as lean_mk_string_from_bytes_unchecked makes")
add("string_get_byte_fast", "string::get_byte_fast", pos_input("inside"),
    f"{{ let p = {RP}; if p < s.len() as u64 {{ u64::from(string::get_byte_fast(s, p)) }} "
    f"else {{ 0 }} }}",
    f"(let p : String.Pos.Raw := {LP}; if h : p < inp.s.rawEndPos then "
    f"(String.getUTF8Byte inp.s p h).toUInt64 else 0)",
    note="both sides make the program's test that supplies the proof")
add("string_utf8_strlen", "string::utf8_strlen", STR, "string::utf8_strlen(black_box(s))",
    "inp.length.toUInt64", comparable=False,
    reason="String.length reads the count cached in the string object, in Lean and in every "
           "translator; utf8_strlen is the count a translator caches when it makes a string", o12=O12_LESS)
STR_PAIR_R = "let (a, b) = (get_str(ss, x >> 60), get_str(ss, (x >> 56) & 15));"
STR_PAIR_L = "let a := inp[(x >>> 60).toNat]!; let b := inp[((x >>> 56) &&& 15).toNat]!;"
STR_NOTE = ("Lean's Array.get! retains and releases the string it reads (an increment and a "
            "decrement of its count); the Rust side borrows it")
add("string_memcmp", "string::memcmp", STRS,
    f"{{ {STR_PAIR_R} let l = x & 63; if l + 512 <= a.len() as u64 && l + 512 <= b.len() as u64 "
    f"{{ string::memcmp(a, b, l, l, 512) as u64 }} else {{ 0 }} }}",
    f"({STR_PAIR_L} let l : String.Pos.Raw := ⟨(x &&& 63).toNat⟩; "
    f"let n : String.Pos.Raw := ⟨512⟩; if h1 : n.offsetBy l ≤ a.rawEndPos then "
    f"if h2 : n.offsetBy l ≤ b.rawEndPos then "
    f"(if String.Slice.Pattern.Internal.memcmpStr a b l l n h1 h2 then 1 else 0) else 0 else 0)",
    note="both sides make the program's tests that supply the proofs; " + STR_NOTE)
add("string_lt", "string::lt", STRS, f"{{ {STR_PAIR_R} string::lt(a, b) as u64 }}",
    f"({STR_PAIR_L} if decide (a < b) then 1 else 0)", note=STR_NOTE)
add("string_compare", "string::compare", STRS,
    f"{{ {STR_PAIR_R} match string::compare(a, b) {{ core::cmp::Ordering::Less => 0, "
    f"core::cmp::Ordering::Equal => 1, core::cmp::Ordering::Greater => 2 }} }}",
    f"({STR_PAIR_L} match compare a b with | .lt => 0 | .eq => 1 | .gt => 2)", note=STR_NOTE)

# Walks over a whole string (from the chunk start 14 * (x & 7)), the hot loops of string code:
# the position is loop-carried, so a step that waits on its byte load shows here.
WALKS = {
    "next": ("string::utf8_next",
             "{ let mut p = nat_box(14 * (x & 7)); let mut k = 0u64; "
             "while !string::utf8_at_end(s, nat_unbox(p)) { "
             "p = nat_box(string::utf8_next(s, nat_unbox(p))); k += 1; } k }",
             "partial def walk (s : String) (p : String.Pos.Raw) (k : UInt64) : UInt64 :=\n"
             "  if String.Pos.Raw.atEnd s p then k else walk s (String.Pos.Raw.next s p) (k + 1)",
             "walk inp ⟨14 * (x &&& 7).toNat⟩ 0",
             "p = next(s, p) from a chunk start to the end"),
    "fold": ("string::utf8_get_fast, string::utf8_next_fast",
             "{ let mut p = nat_box(14 * (x & 7)); let mut a = 0u64; "
             "while !string::utf8_at_end(s, nat_unbox(p)) { "
             "a = a.wrapping_add(u64::from(string::utf8_get_fast(s, nat_unbox(p)))); "
             "p = nat_box(string::utf8_next_fast(s, nat_unbox(p))); } a }",
             "partial def walk (s : String) (p : String.Pos.Raw) (acc : UInt64) : UInt64 :=\n"
             "  if h : String.Pos.Raw.atEnd s p then acc\n"
             "  else walk s (String.Pos.Raw.next' s p h) (acc + (String.Pos.Raw.get' s p h).val.toUInt64)",
             "walk inp ⟨14 * (x &&& 7).toNat⟩ 0",
             "get' and next' from a chunk start to the end, as String.foldl does"),
    "prev": ("string::utf8_prev",
             "{ let mut p = nat_box(s.len() as u64 - 14 * (x & 7)); let mut k = 0u64; "
             "while nat_unbox(p) != 0 { p = nat_box(string::utf8_prev(s, nat_unbox(p))); k += 1; } k }",
             "partial def walk (s : String) (p : String.Pos.Raw) (k : UInt64) : UInt64 :=\n"
             "  if p.byteIdx == 0 then k else walk s (String.Pos.Raw.prev s p) (k + 1)",
             "walk inp ⟨inp.utf8ByteSize - 14 * (x &&& 7).toNat⟩ 0",
             "p = prev(s, p) from a chunk end to 0"),
}
for wk, (fns, rbody, ldef, lcall, what) in WALKS.items():
    for text in ["ascii", "mixed"]:
        add(f"string_walk_{wk}_{text}", fns, WALK[text], rbody, lcall, walk=ldef,
            note=f"one iteration is a whole walk: {what}, over 896 bytes of {text} text")


# ---------------------------------------------------------------- nat, int (semantics batch 2)
# Operands are Nat and Int words drawn from the LCG, small enough that every result stays a word
# on both sides (a big result would be an allocation in Lean and the translator's own backend,
# not this crate): the benchmarks time the rules' word paths. The Rust twin's backend
# (`ColdBig`) has only out-of-line cold methods, never called, as a translator's big path.

NW = {  # Nat words: (Rust, Lean)
    "a30": ("nat_box(x >> 34)", "(x >>> 34).toNat"),
    "b30": ("nat_box((x >> 4) & 0x3FFF_FFFF)", "((x >>> 4) &&& 0x3FFFFFFF).toNat"),
    "b8": ("nat_box((x >> 4) & 0xFF)", "((x >>> 4) &&& 0xFF).toNat"),
    "a62": ("nat_box(x >> 2)", "(x >>> 2).toNat"),
    "b62": ("nat_box(x.rotate_left(17) >> 2)", "(((x <<< 17) ||| (x >>> 47)) >>> 2).toNat"),
    "a63": ("nat_box(x >> 1)", "(x >>> 1).toNat"),
    "p4": ("nat_box(x >> 60)", "(x >>> 60).toNat"),
    "e4": ("nat_box((x >> 4) & 15)", "((x >>> 4) &&& 15).toNat"),
    "a24": ("nat_box(x >> 40)", "(x >>> 40).toNat"),
    "s31": ("nat_box((x >> 4) & 31)", "((x >>> 4) &&& 31).toNat"),
    "s127": ("nat_box((x >> 4) & 127)", "((x >>> 4) &&& 127).toNat"),
    "n31": ("nat_box(x >> 33)", "(x >>> 33).toNat"),
    "c4a": ("nat_box(x >> 60)", "(x >>> 60).toNat"),
    "c4b": ("nat_box((x >> 4) & 15)", "((x >>> 4) &&& 15).toNat"),
}
IW = {  # Int words in the int32 range: (Rust, Lean)
    "a29": ("int_box(((x >> 34) as i64) - (1 << 29))",
            "(((x >>> 34).toUInt32.toInt32) - 536870912).toInt"),
    "b29": ("int_box((((x >> 4) & 0x3FFF_FFFF) as i64) - (1 << 29))",
            "((((x >>> 4) &&& 0x3FFFFFFF).toUInt32.toInt32) - 536870912).toInt"),
    "a15": ("int_box(((x >> 48) as i64) - (1 << 15))",
            "(((x >>> 48).toUInt32.toInt32) - 32768).toInt"),
    "b15": ("int_box((((x >> 4) & 0xFFFF) as i64) - (1 << 15))",
            "((((x >>> 4) &&& 0xFFFF).toUInt32.toInt32) - 32768).toInt"),
    "d8": ("int_box((((x >> 4) & 0xFF) as i64) - 128)",
           "((((x >>> 4) &&& 0xFF).toUInt32.toInt32) - 128).toInt"),
    "c4a": ("int_box(((x >> 60) as i64) - 8)", "(((x >>> 60).toUInt32.toInt32) - 8).toInt"),
    "c4b": ("int_box((((x >> 4) & 15) as i64) - 8)",
            "((((x >>> 4) &&& 15).toUInt32.toInt32) - 8).toInt"),
}
CMP_NOTE = "; operands in 16 values, so equal ones are frequent"
NAT_NOTE = "Nat words on both sides (2n+1); every result stays below 2^63"
INT_NOTE = "Int words in the int32 range on both sides; every result stays there"


def rn(k):
    return f"nat_arg({NW[k][0]})"


def ln_(k):
    return NW[k][1]


def ri(k):
    return f"int_arg({IW[k][0]})"


def li(k):
    return IW[k][1]


def nat_out(e):
    return f"nat_unbox(nat_res({e}))"


def int_out(e):
    return f"int_unbox(int_res({e})) as u64"


for f, lean, args in [("add", "{0} + {1}", ("a30", "b30")), ("sub", "{0} - {1}", ("a30", "b30")),
                      ("mul", "{0} * {1}", ("a30", "b30")), ("div", "{0} / {1}", ("a62", "b8")),
                      ("rem", "{0} % {1}", ("a62", "b8")), ("gcd", "Nat.gcd {0} {1}", ("a30", "b30")),
                      ("land", "{0} &&& {1}", ("a62", "b62")), ("lor", "{0} ||| {1}", ("a62", "b62")),
                      ("lxor", "{0} ^^^ {1}", ("a62", "b62")),
                      ("shiftr", "{0} >>> {1}", ("a62", "s127"))]:
    add(f"nat_{f}", f"nat::{f}", NONE, nat_out(f"nat::{f}({rn(args[0])}, {rn(args[1])})"),
        "(" + lean.format(ln_(args[0]), ln_(args[1])) + ").toUInt64", note=NAT_NOTE)
for f, lean, a in [("succ", "Nat.succ {0}", "a62"), ("pred", "Nat.pred {0}", "a62")]:
    add(f"nat_{f}", f"nat::{f}", NONE, nat_out(f"nat::{f}({rn(a)})"),
        "(" + lean.format(ln_(a)) + ").toUInt64", note=NAT_NOTE)
add("nat_log2", "nat::log2", NONE, f"nat::log2(&{rn('a63')})", f"(Nat.log2 {ln_('a63')}).toUInt64",
    note=NAT_NOTE)
for f, lean, args in [("pow", "{0} ^ {1}", ("p4", "e4")), ("shiftl", "{0} <<< {1}", ("a24", "s31"))]:
    add(f"nat_{f}", f"nat::{f}", NONE,
        f"match nat::{f}({rn(args[0])}, {rn(args[1])}) {{ Ok(v) => nat_unbox(nat_res(v)), "
        f"Err(p) => end(p) }}",
        "(" + lean.format(ln_(args[0]), ln_(args[1])) + ").toUInt64",
        note=NAT_NOTE + "; the limit test (an exponent of 2^32 or more) is made on both sides")
add("nat_div_exact", "nat::div_exact, nat::mul, nat::rem", NONE,
    f"{{ let b = {NW['b8'][0]}; let a = nat_res(nat::mul({rn('a30')}, nat_arg(b))); "
    f"if nat::rem(nat_arg(a), nat_arg(b)).is_zero() "
    f"{{ nat_unbox(nat_res(nat::div_exact(nat_arg(a), nat_arg(b)))) }} else {{ 0 }} }}",
    f"(let b := {ln_('b8')}; let a := {ln_('a30')} * b; "
    f"if h : b ∣ a then (Nat.divExact a b h).toUInt64 else 0)",
    note=NAT_NOTE + "; both sides make the program's test that supplies the proof (`b ∣ a`, "
         "a remainder)")
for f, lean in [("dec_eq", "{0} = {1}"), ("dec_lt", "{0} < {1}"), ("dec_le", "{0} ≤ {1}")]:
    add(f"nat_{f}", f"nat::{f}", NONE, f"nat::{f}(&{rn('c4a')}, &{rn('c4b')}) as u64",
        "(if " + lean.format(ln_("c4a"), ln_("c4b")) + " then 1 else 0)", note=NAT_NOTE + CMP_NOTE)
add("nat_compare", "nat::compare", NONE,
    f"match nat::compare(&{rn('c4a')}, &{rn('c4b')}) {{ core::cmp::Ordering::Less => 0, "
    f"core::cmp::Ordering::Equal => 1, core::cmp::Ordering::Greater => 2 }}",
    f"(match compare {ln_('c4a')} {ln_('c4b')} with | .lt => 0 | .eq => 1 | .gt => 2)",
    note=NAT_NOTE + CMP_NOTE)
add("nat_write_decimal", "nat::write_decimal", NONE,
    f"{{ let mut b = StackBuf::new(); let _ = nat::write_decimal(&{rn('a63')}, &mut b); "
    f"make_ascii_string(b.as_bytes()).byte_size() }}",
    f"(Nat.repr {ln_('a63')}).utf8ByteSize.toUInt64",
    note="values of 2^62 or so, so Nat.repr takes lean_string_of_usize, never the shared strings "
         "below 128; the Rust twin formats on the stack and makes one new string object")
# word helpers
for f, lean, args, wide in [("add_small", "{0} + {1}", ("a30", "b30"), True),
                            ("mul_small", "{0} * {1}", ("a30", "b30"), True),
                            ("sub_small", "{0} - {1}", ("a30", "b30"), False),
                            ("div_small", "{0} / {1}", ("a62", "b8"), False),
                            ("mod_small", "{0} % {1}", ("a62", "b8"), False),
                            ("gcd_small", "Nat.gcd {0} {1}", ("a30", "b30"), False),
                            ("shiftr_small", "{0} >>> {1}", ("a62", "s127"), False)]:
    call = f"nat::{f}(nat_unbox({NW[args[0]][0]}), nat_unbox({NW[args[1]][0]}))"
    rust = (f"{{ let r = {call}; nat_unbox(if r >> 63 == 0 {{ nat_box(r as u64) }} else {{ big() }}) }}"
            if wide else f"nat_unbox(nat_box({call}))")
    add(f"nat_{f}", f"nat::{f}", NONE, rust,
        "(" + lean.format(ln_(args[0]), ln_(args[1])) + ").toUInt64",
        note=NAT_NOTE + "; the word helper on unboxed words, the glue's own fast path")
for f, lean, args in [("pow_small", "{0} ^ {1}", ("p4", "e4")),
                      ("shiftl_small", "{0} <<< {1}", ("a24", "s31"))]:
    add(f"nat_{f}", f"nat::{f}", NONE,
        f"match nat::{f}(nat_unbox({NW[args[0]][0]}), nat_unbox({NW[args[1]][0]})) "
        f"{{ Some(v) => nat_unbox(nat_box(v)), None => big() }}",
        "(" + lean.format(ln_(args[0]), ln_(args[1])) + ").toUInt64",
        note=NAT_NOTE + "; the word helper on unboxed words (the limit test is the caller's)")
add("nat_log2_small", "nat::log2_small", NONE, f"nat::log2_small(nat_unbox({NW['a63'][0]}))",
    f"(Nat.log2 {ln_('a63')}).toUInt64", note=NAT_NOTE)
for f, small, lean, args in [("pow_exponent", "pow_small", "{0} ^ {1}", ("p4", "e4")),
                             ("shiftl_amount", "shiftl_small", "{0} <<< {1}", ("a24", "s31"))]:
    add(f"nat_{f}", f"nat::{f}, nat::{small}", NONE,
        f"match nat::{f}(&{rn(args[1])}) {{ Ok(e) => match nat::{small}(nat_unbox({NW[args[0]][0]}), "
        f"e as u64) {{ Some(v) => nat_unbox(nat_box(v)), None => big() }}, Err(p) => end(p) }}",
        "(" + lean.format(ln_(args[0]), ln_(args[1])) + ").toUInt64",
        note=NAT_NOTE + f"; the limit test, then the word helper, as a translator's fast path "
             f"composes them")

for f, lean, args in [("add", "{0} + {1}", ("a29", "b29")), ("sub", "{0} - {1}", ("a29", "b29")),
                      ("mul", "{0} * {1}", ("a15", "b15")),
                      ("tdiv", "Int.tdiv {0} {1}", ("a29", "d8")),
                      ("tmod", "Int.tmod {0} {1}", ("a29", "d8")),
                      ("ediv", "{0} / {1}", ("a29", "d8")), ("emod", "{0} % {1}", ("a29", "d8"))]:
    add(f"int_{f}", f"int::{f}", NONE, int_out(f"int::{f}({ri(args[0])}, {ri(args[1])})"),
        "(" + lean.format(li(args[0]), li(args[1])) + ").toInt64.toUInt64", note=INT_NOTE)
add("int_neg", "int::neg", NONE, int_out(f"int::neg({ri('a29')})"),
    f"(-{li('a29')}).toInt64.toUInt64", note=INT_NOTE)
add("int_div_exact", "int::div_exact, int::mul, int::emod", NONE,
    f"{{ let b = {IW['d8'][0]}; let a = int_res(int::mul({ri('a15')}, int_arg(b))); "
    f"if int::emod(int_arg(a), int_arg(b)).is_zero() "
    f"{{ int_unbox(int_res(int::div_exact(int_arg(a), int_arg(b)))) as u64 }} else {{ 0 }} }}",
    f"(let b := {li('d8')}; let a := {li('a15')} * b; "
    f"if h : b ∣ a then (Int.divExact a b h).toInt64.toUInt64 else 0)",
    note=INT_NOTE + "; both sides make the program's test that supplies the proof (`b ∣ a`, a "
         "remainder)")
for f, lean in [("dec_eq", "{0} = {1}"), ("dec_lt", "{0} < {1}"), ("dec_le", "{0} ≤ {1}")]:
    add(f"int_{f}", f"int::{f}", NONE, f"int::{f}(&{ri('c4a')}, &{ri('c4b')}) as u64",
        "(if " + lean.format(li("c4a"), li("c4b")) + " then 1 else 0)", note=INT_NOTE + CMP_NOTE)
add("int_compare", "int::compare", NONE,
    f"match int::compare(&{ri('c4a')}, &{ri('c4b')}) {{ core::cmp::Ordering::Less => 0, "
    f"core::cmp::Ordering::Equal => 1, core::cmp::Ordering::Greater => 2 }}",
    f"(match compare {li('c4a')} {li('c4b')} with | .lt => 0 | .eq => 1 | .gt => 2)",
    note=INT_NOTE + CMP_NOTE)
add("int_dec_nonneg", "int::dec_nonneg", NONE, f"int::dec_nonneg(&{ri('a29')}) as u64",
    f"(Int.decNonneg {li('a29')}).decide.toUInt64", note=INT_NOTE)
add("int_of_nat", "int::of_nat", NONE, int_out(f"int::of_nat::<ColdBig>({rn('n31')})"),
    f"(Int.ofNat {ln_('n31')}).toInt64.toUInt64", note="Nat words below 2^31, so the Int is a word")
add("int_neg_succ_of_nat", "int::neg_succ_of_nat", NONE,
    int_out(f"int::neg_succ_of_nat::<ColdBig>({rn('a30')})"),
    f"(Int.negSucc {ln_('a30')}).toInt64.toUInt64", note="Nat words below 2^30, so the Int is a word")
add("int_nat_abs", "int::nat_abs", NONE, nat_out(f"int::nat_abs({ri('a29')})"),
    f"(Int.natAbs {li('a29')}).toUInt64", note=INT_NOTE)
add("int_write_decimal", "int::write_decimal", NONE,
    f"{{ let mut b = StackBuf::new(); let _ = int::write_decimal(&{ri('a29')}, &mut b); "
    f"make_ascii_string(b.as_bytes()).byte_size() }}",
    f"(Int.repr {li('a29')}).utf8ByteSize.toUInt64",
    note="Int.repr is Lean code: a negative value costs native two strings and an append, where a "
         "translator replaces it with one formatting call (on the stack, then one new string "
         "object)")
for f, lean, args, wide in [("add_small", "{0} + {1}", ("a29", "b29"), True),
                            ("sub_small", "{0} - {1}", ("a29", "b29"), True),
                            ("mul_small", "{0} * {1}", ("a15", "b15"), True),
                            ("tdiv_small", "Int.tdiv {0} {1}", ("a29", "d8"), True),
                            ("ediv_small", "{0} / {1}", ("a29", "d8"), True),
                            ("tmod_small", "Int.tmod {0} {1}", ("a29", "d8"), False),
                            ("emod_small", "{0} % {1}", ("a29", "d8"), False)]:
    call = f"int::{f}(int_unbox({IW[args[0]][0]}), int_unbox({IW[args[1]][0]}))"
    rust = (f"{{ let r = {call}; int_unbox(match i64::try_from(r) {{ Ok(v) => int_box(v), "
            f"Err(_) => big() }}) as u64 }}" if wide else f"int_unbox(int_box({call})) as u64")
    add(f"int_{f}", f"int::{f}", NONE, rust,
        "(" + lean.format(li(args[0]), li(args[1])) + ").toInt64.toUInt64",
        note=INT_NOTE + "; the word helper on unboxed words, the glue's own fast path")
add("int_neg_small", "int::neg_small", NONE,
    f"{{ let r = int::neg_small(int_unbox({IW['a29'][0]})); int_unbox(match i64::try_from(r) "
    f"{{ Ok(v) => int_box(v), Err(_) => big() }}) as u64 }}",
    f"(-{li('a29')}).toInt64.toUInt64", note=INT_NOTE + "; the word helper on unboxed words")

# ---------------------------------------------------------------- array (semantics batch 2)

NATS = ("Vec<u64>", "nat_words()", "let xs = &inp[..];", "Array Nat", "pure natOperands")
BYTES4K = ("Vec<u8>", "byte_operands()", "let b = &inp[..];", "ByteArray", "pure byteOperands")
SRC = ("Vec<u8>", "src_bytes()", "let src = &inp[..];", "ByteArray", "pure srcBytes")
IDX = "nat_unbox(nat_box(x >> 52))"   # an index in bounds of a 4096-element array
LIDX = "(x >>> 52).toNat"
OBS = "nat_unbox(get_word(&st, (x >> 40) & 4095))"
LOBS = "(st[((x >>> 40) &&& 4095).toNat]!).toUInt64"
ARR_NOTE = ("the array is updated in place on both sides (Lean's is unique after its first "
            "copy of the input, the Rust twin's a copy made once)")

add("array_get_bang", "array::get_bang", NATS,
    f"match array::get_bang(xs.len(), {IDX}) {{ Ok(k) => nat_unbox(xs[k]), "
    f"Err(o) => {{ lean_panic(o.message()); 0 }} }}",
    f"(inp[{LIDX}]!).toUInt64",
    note="indices in bounds: out of bounds both sides print a panic message")
add("array_set_bang", "array::set_bang", NATS, OBS, LOBS, note=ARR_NOTE,
    state=("inp.clone()",
           f"match array::set_bang(st.len(), {IDX}) {{ Ok(k) => st[k] = nat_box(x & 0xFFFF), "
           f"Err(o) => lean_panic(o.message()) }}",
           "Array Nat", "inp", f"st.set! {LIDX} (x &&& 0xFFFF).toNat"))
add("array_swap_if_in_bounds", "array::swap_if_in_bounds", NATS, OBS, LOBS,
    note=ARR_NOTE + "; the second index is out of bounds half the time (no message)",
    state=("inp.clone()",
           f"if let Some((i, j)) = array::swap_if_in_bounds(st.len(), {IDX}, "
           f"nat_unbox(nat_box((x >> 39) & 8191))) {{ st.swap(i, j); }}",
           "Array Nat", "inp", f"st.swapIfInBounds {LIDX} ((x >>> 39) &&& 8191).toNat"))
add("array_pop", "array::pop", NATS, OBS, LOBS,
    note=ARR_NOTE + "; each iteration pops and pushes one element",
    state=("inp.clone()",
           "if let Some(k) = array::pop(st.len()) { st.truncate(k); } st.push(nat_box(x >> 40));",
           "Array Nat", "inp", "st.pop.push (x >>> 40).toNat"))
add("array_byte_array_get", "array::byte_array_get", BYTES4K,
    "array::byte_array_get(b, nat_unbox(nat_box(x >> 51))) as u64",
    "(inp.get! (x >>> 51).toNat).toUInt64",
    note="half the indices are out of bounds (0, no message)")
add("array_byte_array_set", "array::byte_array_set", BYTES4K,
    "u64::from(st[((x >> 40) & 4095) as usize])", "(st.get! ((x >>> 40) &&& 4095).toNat).toUInt64",
    note=ARR_NOTE + "; half the indices are out of bounds (no message)",
    state=("inp.clone()",
           "if let Some(k) = array::byte_array_set(st.len(), nat_unbox(nat_box(x >> 51))) "
           "{ st[k] = x as u8; }",
           "ByteArray", "inp", "st.set! (x >>> 51).toNat x.toUInt8"))
FLOATS = f64_input(-1e6, 2e6, False)
add("array_float_array_get", "array::float_array_get", FLOATS,
    "fbits(array::float_array_get(xs, nat_unbox(nat_box(x >> 51))))",
    "(inp.get! (x >>> 51).toNat).toBits",
    note="half the indices are out of bounds (0.0, no message)")
add("array_float_array_set", "array::float_array_set", FLOATS,
    "fbits(st[((x >> 40) & 4095) as usize])", "(st.get! ((x >>> 40) &&& 4095).toNat).toBits",
    note=ARR_NOTE + "; half the indices are out of bounds (no message)",
    state=("inp.clone()",
           "if let Some(k) = array::float_array_set(st.len(), nat_unbox(nat_box(x >> 51))) "
           "{ st[k] = (x >> 12) as f64; }",
           "FloatArray", "inp", "st.set! (x >>> 51).toNat (x >>> 12).toFloat"))
add("array_alloc_bytes", "array::alloc_bytes", NONE,
    "match array::alloc_bytes(array::BYTE_ELEMENT_BYTES, nat_unbox(nat_box((x >> 4) & 0xFF))) "
    "{ Ok(bytes) => black_box(new_object(bytes)).len() as u64 - 24, Err(p) => end(p) }",
    "(ByteArray.emptyWithCapacity ((x >>> 4) &&& 0xFF).toNat).size.toUInt64",
    note="ByteArray.emptyWithCapacity: one allocation of the object's 24 + c bytes on both sides")
add("array_empty_with_capacity", "array::empty_with_capacity", NONE,
    "match array::empty_with_capacity(array::WORD_ELEMENT_BYTES, nat_unbox(nat_box((x >> 4) & 0xFF))) "
    "{ Ok(c) => black_box(new_object(24 + 8 * c as u64)).len() as u64 - 24, Err(p) => end(p) }",
    "(Array.mkEmpty (α := Nat) ((x >>> 4) &&& 0xFF).toNat).size.toUInt64",
    note="Array.mkEmpty: one allocation of the object's 24 + 8c bytes on both sides")
add("array_replicate_len", "array::replicate_len", NONE,
    "match array::replicate_len(nat_arg(nat_box((x >> 4) & 15)).to_u64()) { Ok(k) => { "
    "let mut v = Vec::<u64>::with_capacity(3 + k); v.extend_from_slice(&[0; 3]); "
    "v.resize(3 + k, nat_box(x >> 40)); (black_box(v).len() - 3) as u64 } Err(p) => end(p) }",
    "(Array.replicate ((x >>> 4) &&& 15).toNat (x >>> 40).toNat).size.toUInt64",
    note="Array.replicate: one allocation of the object's 24 + 8n bytes, n words stored, on both "
         "sides")
add("array_copy_slice", "array::copy_slice", SRC,
    "st.len() as u64 + u64::from(st[((x >> 40) & 63) as usize])",
    "(st.size.toUInt64 + (st.get! ((x >>> 40) &&& 63).toNat).toUInt64)",
    note=ARR_NOTE + "; offsets anywhere in the 64-byte source and destination, lengths up to 15, "
         "so the destination grows to at most 78 bytes",
    state=("inp.clone()",
           "if let Some(p) = array::copy_slice(src.len(), nat_unbox(nat_box(x >> 58)), st.len(), "
           "nat_unbox(nat_box((x >> 52) & 63)), nat_unbox(nat_box((x >> 46) & 15))) { "
           "if st.len() < p.new_len { st.resize(p.new_len, 0); } "
           "st[p.dest_start..p.dest_start + p.len]"
           ".copy_from_slice(&src[p.src_start..p.src_start + p.len]); }",
           "ByteArray", "inp",
           "ByteArray.copySlice inp (x >>> 58).toNat st ((x >>> 52) &&& 63).toNat "
           "((x >>> 46) &&& 15).toNat"))

# ---------------------------------------------------------------- repr (semantics batch 2)

CHARS = ("Vec<char>", "char_operands()", "let cs = &inp[..];", "Array Char", "pure charOperands")
QSTRS = ("Vec<String>", "quote_strings()", "let ss = &inp[..];", "Array String", "pure quoteStrings")
TEXT_NOTE = ("Lean code natively (small strings and appends); the Rust twin formats on the stack "
             "and makes one new string object, its characters counted")

add("repr_decimal_u64", "repr::decimal_u64", NONE,
    f"{{ let mut buf = [0u8; 20]; "
    f"make_ascii_string(repr::decimal_u64(nat_unbox({NW['a63'][0]}), &mut buf).as_bytes())"
    f".byte_size() }}",
    f"(Nat.repr {ln_('a63')}).utf8ByteSize.toUInt64",
    note="values of 2^62 or so (lean_string_of_usize); one new string object on both sides")
add("repr_usize_repr", "repr::usize_repr", NONE,
    "{ let mut b = StackBuf::new(); let _ = repr::usize_repr(x, &mut b); "
    "make_ascii_string(b.as_bytes()).byte_size() }",
    "(USize.repr x.toUSize).utf8ByteSize.toUInt64",
    note="lean_string_of_usize; one new string object on both sides")
add("repr_bool_text", "repr::bool_text", NONE, "repr::bool_text(x & 1 == 1).len() as u64",
    "(toString (x &&& 1 == 1)).utf8ByteSize.toUInt64",
    note="a constant string on both sides (no allocation)")
add("repr_needs_app_paren", "repr::needs_app_paren, int::write_decimal", NONE,
    f"{{ let i = {ri('a29')}; let mut b = StackBuf::new(); let _ = int::write_decimal(&i, &mut b); "
    f"b.as_bytes().len() as u64 + 2 * repr::needs_app_paren(i.is_neg(), "
    f"(x >> 4) as u32 & 2047) as u64 }}",
    f"(reprPrec {li('a29')} ((x >>> 4) &&& 2047).toNat).pretty.utf8ByteSize.toUInt64",
    comparable=False,
    reason="Repr.addAppParen builds a Format (a tree the program renders later); the crate only "
           "answers its test",
    o12="by inspection: two comparisons, which Repr.addAppParen makes as well")
for f, lean in [("char_quote_core", "Char.quoteCore {0}"), ("char_quote", "Char.quote {0}"),
                ("char_to_string", "Char.toString {0}")]:
    call = ("repr::char_quote_core(c, false, &mut b)" if f == "char_quote_core"
            else f"repr::{f}(c, &mut b)")
    add(f"repr_{f}", f"repr::{f}", CHARS,
        f"{{ let c = get_char(cs, x >> 60); let mut b = StackBuf::new(); let _ = {call}; "
        f"make_string(b.as_bytes()).byte_size() }}",
        "(" + lean.format("inp[(x >>> 60).toNat]!") + ").utf8ByteSize.toUInt64",
        note=TEXT_NOTE + "; 16 characters, escapes among them")
add("repr_string_quote", "repr::string_quote", QSTRS,
    "{ let s = get_string(ss, x >> 60); let mut b = StackBuf::new(); "
    "let _ = repr::string_quote(s, &mut b); make_string(b.as_bytes()).byte_size() }",
    "(String.quote inp[(x >>> 60).toNat]!).utf8ByteSize.toUInt64",
    note=TEXT_NOTE + "; 16 strings of 30 to 60 bytes with escapes")


# ---------------------------------------------------------------- writers


def camel(name):
    return "".join(p[:1].upper() + p[1:] for p in name.split("_"))


def wrap(text, width=94):
    words, lines, cur = text.split(), [], ""
    for w in words:
        if cur and len(cur) + 1 + len(w) > width:
            lines.append(cur)
            cur = w
        else:
            cur = f"{cur} {w}" if cur else w
    if cur:
        lines.append(cur)
    return lines


def rust_file(b):
    rty, rbuild, rpre, _, _ = b.inp
    mods = sorted({m.split("::")[0].strip() for m in b.fn.split(", ")})
    note = "".join(f"\n//! {line}" for line in wrap(b.note)) if b.note else ""
    if not b.comparable:
        note += "".join(f"\n//! {line}" for line in wrap("NOT COMPARABLE: " + b.reason))
    upd = f"acc = {b.rust};" if b.shape == "chain" else f"acc = mix(acc, {b.rust});"
    init = ""
    if b.state:
        rinit, rupd = b.state[0], b.state[1]
        init = f"\n    let mut st = {rinit};"
        upd = f"{rupd}\n        {upd}"
    return f"""//! Generated by `scripts/gen_benches.py`: times `lean_runtime::semantics::{b.fn}`; the
//! native twin is `benches/native/Bench/{camel(b.name)}.lean`.{note}

#![allow(unused_parens, unused_imports, unused_variables, unused_mut)]
#![allow(clippy::double_parens, clippy::unnecessary_cast, clippy::redundant_closure)]
#![allow(clippy::identity_op, clippy::let_and_return)]

use lean_runtime::semantics::{{{", ".join(mods)}}};
use lean_runtime_bench::*;
use std::hint::black_box;

#[inline(never)]
fn kernel(inp: &{rty}, n: u64) -> u64 {{
    {rpre}{init}
    let (mut x, mut acc) = (SEED, 0u64);
    for _ in 0..n {{
        x = step(x);
        {upd}
    }}
    acc
}}

fn main() {{
    run(|| {rbuild}, kernel)
}}
"""


def lean_file(b):
    _, _, _, lty, lbuild = b.inp
    ns = f"Bench.{camel(b.name)}"
    note = "\n" + "\n".join(wrap(b.note)) if b.note else ""
    if not b.comparable:
        note += "\n" + "\n".join(wrap("NOT COMPARABLE: " + b.reason))
    upd = b.lean if b.shape == "chain" else f"mix acc ({b.lean})"
    walk = f"\n{b.walk}\n" if b.walk else ""
    if b.state:
        _, _, sty, linit, lupd = b.state
        loop = (f"partial def loop (inp : {lty}) (st : {sty}) (n i x acc : UInt64) : UInt64 :=\n"
                f"  if i < n then\n    let x := step x\n    let st := {lupd}\n"
                f"    loop inp st n (i + 1) x ({upd})\n  else acc\n\n"
                f"def kernel (inp : {lty}) (n : UInt64) : UInt64 :=\n  loop inp ({linit}) n 0 SEED 0")
    else:
        loop = (f"partial def loop (inp : {lty}) (n i x acc : UInt64) : UInt64 :=\n"
                f"  if i < n then\n    let x := step x\n    loop inp n (i + 1) x ({upd})\n"
                f"  else acc\n\n"
                f"def kernel (inp : {lty}) (n : UInt64) : UInt64 :=\n  loop inp n 0 SEED 0")
    return f"""/-
Generated by `scripts/gen_benches.py`: the native twin of `benches/rust/src/bin/{b.name}.rs`,
timing what `lean_runtime::semantics::{b.fn}` mirrors, through Lean's API.{note}
-/
import Bench.Harness

open Bench

namespace {ns}
{walk}
{loop}

end {ns}

def main (args : List String) : IO UInt32 :=
  Bench.run (fun _ => {lbuild}) {ns}.kernel args
"""


def toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


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
        'name = "bench"\ndefaultTargets = [' + ", ".join(f'"{b.name}"' for b in BENCHES) +
        ']\n\n[[lean_lib]]\nname = "Bench"\nroots = ["Bench.Harness"]\n' + exes)
    manifest = ["# Generated by scripts/gen_benches.py: every benchmark pair (benches/README.md).",
                ""]
    for b in BENCHES:
        manifest += ["[[bench]]", f"name = {toml_str(b.name)}", f"function = {toml_str(b.fn)}",
                     f"comparable = {'true' if b.comparable else 'false'}"]
        if not b.comparable:
            manifest.append(f"reason = {toml_str(b.reason)}")
        if b.o12:
            manifest.append(f"o12 = {toml_str(b.o12)}")
        if b.note:
            manifest.append(f"note = {toml_str(b.note)}")
        manifest.append("")
    (ROOT / "benches" / "benches.toml").write_text("\n".join(manifest))
    subprocess.run(["rustfmt", "--edition", "2021"] + [str(bins / f"{b.name}.rs") for b in BENCHES],
                   check=True)
    print(f"{len(BENCHES)} benchmarks")


if __name__ == "__main__":
    main()
