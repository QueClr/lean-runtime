//! Runs every row of `tests/cases/<area>/<area>.rows.toml` (expected values
//! from native Lean 4.34.0, see `tests/cases/README.md`) against the crate,
//! for the areas hash, float, libm, uint, sint, string, net and toolchain
//! (`tests/rows2.rs` runs the others).
//!
//! Each Lean function maps to the crate's function plus the small amount of
//! glue a translator writes around it: `Nat`/`Int` arguments reduced to the
//! `u64` the crate takes, big positions handled by `Nat` arithmetic, the
//! panic of `String.Pos.Raw.get!` reported, and the result rendered as Lean's
//! `repr`, with the bits of a float result.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use common::{Toml, Value};
use lean_runtime::semantics::{float, float32, hash, libm, net, sint, string, toolchain, uint};

mod common;

// ------------------------------------------------------------------ rows

#[derive(Clone, Debug)]
enum Arg {
    Nat(u128),
    Int(i128),
    Str(String),
    Bytes(Vec<u8>),
    F64(f64),
    F32(f32),
    Char(char),
    /// `((s.toSlice.drop d).dropEnd e)`
    Slice(String, usize, usize),
}

struct Row {
    id: String,
    func: String,
    args: Vec<Arg>,
    expected: String,
    default: Option<String>,
    stderr: Option<String>,
    result_bits: Option<String>,
}

fn parse_nat(text: &str) -> Option<(u128, usize)> {
    if let Some(hex) = text.strip_prefix("0x") {
        let n = hex.bytes().take_while(u8::is_ascii_hexdigit).count();
        Some((u128::from_str_radix(&hex[..n], 16).ok()?, n + 2))
    } else {
        let n = text.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        Some((text[..n].parse().ok()?, n))
    }
}

/// A Lean string literal at the start of `text`: the string and its length.
fn parse_string_literal(text: &str) -> Result<(String, usize), String> {
    parse_literal(text, '"')
}

/// A Lean string or character literal at the start of `text` (after its
/// opening `quote`): the text and the literal's length.
fn parse_literal(text: &str, quote: char) -> Result<(String, usize), String> {
    let mut chars = text.char_indices();
    assert_eq!(chars.next().map(|c| c.1), Some(quote));
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            c if c == quote => return Ok((out, i + c.len_utf8())),
            '\\' => {
                let (_, e) = chars.next().ok_or("unterminated escape")?;
                let hex = |chars: &mut std::str::CharIndices, n: usize| -> Result<char, String> {
                    let s: String = (0..n).filter_map(|_| chars.next().map(|c| c.1)).collect();
                    u32::from_str_radix(&s, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or(format!("bad escape \\{e}{s}"))
                };
                out.push(match e {
                    '\\' => '\\',
                    '"' => '"',
                    '\'' => '\'',
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'x' => hex(&mut chars, 2)?,
                    'u' => hex(&mut chars, 4)?,
                    _ => return Err(format!("unknown escape \\{e}")),
                });
            }
            c => out.push(c),
        }
    }
    Err("unterminated string literal".into())
}

/// Whether an argument term denotes a float (its value is then the next
/// entry of `bits.args`): a decimal literal, maybe negated and parenthesized,
/// or `0.0 / 0.0`, `1.0 / 0.0` and their negations.
fn is_float_term(term: &str) -> bool {
    let mut t = term.trim();
    loop {
        if let Some(inner) = t.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            t = inner.trim();
        } else if let Some(rest) = t.strip_prefix('-') {
            t = rest.trim();
        } else {
            break;
        }
    }
    if t == "0.0 / 0.0" || t == "1.0 / 0.0" {
        return true;
    }
    let (mant, exp) = match t.split_once('e') {
        Some((m, e)) => (m, Some(e.strip_prefix('-').unwrap_or(e))),
        None => (t, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mant_ok = match mant.split_once('.') {
        Some((a, b)) => digits(a) && digits(b),
        None => digits(mant) && exp.is_some(),
    };
    mant_ok && exp.is_none_or(digits)
}

fn parse_arg(term: &str, float_bits: &mut std::vec::IntoIter<String>) -> Result<Arg, String> {
    let t = term.trim();
    if is_float_term(t) {
        let b = float_bits.next().ok_or(format!("no bits for {t}"))?;
        let hex = b.strip_prefix("0x").ok_or("bits without 0x")?;
        let n = u64::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
        return match hex.len() {
            // the exact bits, a negative NaN included (`of_bits` would make it the quiet NaN)
            16 => Ok(Arg::F64(f64::from_bits(n))),
            8 => Ok(Arg::F32(f32::from_bits(n as u32))),
            _ => Err(format!("bits of 8 or 16 hex digits: {b}")),
        };
    }
    let whole = |r: Result<(Arg, usize), String>| -> Result<Arg, String> {
        let (a, n) = r?;
        if n == t.len() {
            Ok(a)
        } else {
            Err(format!("trailing text in {t}"))
        }
    };
    if t.starts_with('"') {
        return whole(parse_string_literal(t).map(|(s, n)| (Arg::Str(s), n)));
    }
    if t.starts_with('\'') {
        let (s, n) = parse_literal(t, '\'')?;
        let mut cs = s.chars();
        return match (cs.next(), cs.next()) {
            (Some(c), None) => whole(Ok((Arg::Char(c), n))),
            _ => Err(format!("one character: {t}")),
        };
    }
    if let Some(rest) = t.strip_prefix("((") {
        let (s, n) = parse_string_literal(rest)?;
        let rest = rest[n..]
            .strip_prefix(".toSlice.drop ")
            .ok_or("bad slice")?;
        let (d, n1) = parse_nat(rest).ok_or("bad slice drop")?;
        let rest = rest[n1..].strip_prefix(").dropEnd ").ok_or("bad slice")?;
        let (e, n2) = parse_nat(rest).ok_or("bad slice dropEnd")?;
        if &rest[n2..] != ")" {
            return Err(format!("bad slice: {t}"));
        }
        return Ok(Arg::Slice(s, d as usize, e as usize));
    }
    if let Some(inner) = t.strip_prefix('⟨').and_then(|t| t.strip_suffix('⟩')) {
        return whole(
            parse_nat(inner)
                .map(|(n, _)| (Arg::Nat(n), t.len()))
                .ok_or("bad position".into()),
        );
    }
    if let Some(inner) = t
        .strip_prefix("(ByteArray.mk #[")
        .and_then(|t| t.strip_suffix("])"))
    {
        let bytes = inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| parse_nat(s).map(|(n, _)| n as u8).ok_or("bad byte"))
            .collect::<Result<Vec<u8>, _>>()?;
        return Ok(Arg::Bytes(bytes));
    }
    if let Some(inner) = t.strip_prefix("(-").and_then(|t| t.strip_suffix(')')) {
        let (n, k) = parse_nat(inner).ok_or("bad negative numeral")?;
        return whole(Ok((Arg::Int(-(n as i128)), k + 3)));
    }
    whole(
        parse_nat(t)
            .map(|(n, k)| (Arg::Nat(n), k))
            .ok_or(format!("cannot parse argument {t}")),
    )
}

fn read_rows(file: &str, text: &str) -> Vec<Row> {
    let tables = Toml::rows(text).unwrap_or_else(|e| panic!("{file}: {e}"));
    tables
        .iter()
        .map(|t| {
            let field = |k: &str| t.get(k).map(|v| v.as_str().to_string());
            let id = field("id").expect("a row has an id");
            let func = field("fn").unwrap_or_else(|| panic!("{id}: only fn + args rows"));
            let bits = t.get("bits");
            let mut float_bits = bits
                .and_then(|b| b.get("args"))
                .map(Value::strings)
                .unwrap_or_default()
                .into_iter();
            let args = t
                .get("args")
                .map(Value::strings)
                .unwrap_or_default()
                .iter()
                .map(|a| parse_arg(a, &mut float_bits))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|e| panic!("{file}: {id}: {e}"));
            assert!(float_bits.next().is_none(), "{id}: unused bits.args");
            Row {
                func,
                args,
                expected: field("expected").expect("a row has an expected value"),
                default: field("default"),
                stderr: field("stderr"),
                result_bits: bits
                    .and_then(|b| b.get("result"))
                    .map(|v| v.as_str().to_string()),
                id,
            }
        })
        .collect()
}

// ------------------------------------------------------------------ rendering, as Lean's repr

fn quote_core(out: &mut String, c: char, in_string: bool) {
    match c {
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\\' => out.push_str("\\\\"),
        '"' => out.push_str("\\\""),
        '\'' if !in_string => out.push_str("\\'"),
        c if (c as u32) <= 31 || c == '\x7f' => {
            let _ = write!(out, "\\x{:02x}", c as u32);
        }
        c => out.push(c),
    }
}

fn char_repr(c: u32) -> String {
    let mut out = String::from("'");
    quote_core(
        &mut out,
        char::from_u32(c).expect("a Char is a scalar value"),
        false,
    );
    out.push('\'');
    out
}

fn str_repr(bytes: &[u8]) -> String {
    let s = std::str::from_utf8(bytes).expect("a String is valid UTF-8");
    let mut out = String::from("\"");
    for c in s.chars() {
        quote_core(&mut out, c, true);
    }
    out.push('"');
    out
}

/// `repr` of a `Float` at precedence 0: `Float.toString`.
fn f64_str(x: f64) -> String {
    let mut s = String::new();
    float::to_string(x, &mut s).unwrap();
    s
}

/// `repr` of a `Float32` at precedence 0: `Float32.toString`.
fn f32_str(x: f32) -> String {
    let mut s = String::new();
    float32::to_string(x, &mut s).unwrap();
    s
}

fn pos_repr(p: u128) -> String {
    format!("{{ byteIdx := {p} }}")
}

fn ordering_repr(o: Ordering) -> String {
    match o {
        Ordering::Less => "Ordering.lt",
        Ordering::Equal => "Ordering.eq",
        Ordering::Greater => "Ordering.gt",
    }
    .to_string()
}

// ------------------------------------------------------------------ argument glue

fn nat(a: &Arg) -> u128 {
    match a {
        Arg::Nat(n) => *n,
        _ => panic!("expected a Nat, got {a:?}"),
    }
}

fn int(a: &Arg) -> i128 {
    match a {
        Arg::Nat(n) => *n as i128,
        Arg::Int(i) => *i,
        _ => panic!("expected an Int, got {a:?}"),
    }
}

/// A `Nat` modulo 2^64, as a translator passes it to `ofNat`.
fn nat_low64(a: &Arg) -> u64 {
    nat(a) as u64
}

/// An `Int` modulo 2^64 in two's complement, as a translator passes it to
/// `ofInt` (and as a fixed-width literal's value).
fn int_low64(a: &Arg) -> u64 {
    int(a) as u64
}

/// An `Int` saturated to `i64`, as a translator passes it to `scaleB`.
fn int_sat(a: &Arg) -> i64 {
    int(a).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn f64a(a: &Arg) -> f64 {
    match a {
        Arg::F64(x) => *x,
        _ => panic!("expected a Float, got {a:?}"),
    }
}

fn f32a(a: &Arg) -> f32 {
    match a {
        Arg::F32(x) => *x,
        _ => panic!("expected a Float32, got {a:?}"),
    }
}

fn chr(a: &Arg) -> u32 {
    match a {
        Arg::Char(c) => *c as u32,
        _ => panic!("expected a Char, got {a:?}"),
    }
}

fn bytes(a: &Arg) -> &[u8] {
    match a {
        Arg::Str(s) => s.as_bytes(),
        Arg::Bytes(b) => b,
        _ => panic!("expected a String, got {a:?}"),
    }
}

/// The bytes of `(s.toSlice.drop d).dropEnd e` (characters, not bytes).
fn slice(a: &Arg) -> &[u8] {
    match a {
        Arg::Slice(s, d, e) => {
            let start = s.char_indices().nth(*d).map_or(s.len(), |(i, _)| i);
            let rest = &s[start..];
            let n = rest.chars().count();
            let end = start
                + rest
                    .char_indices()
                    .nth(n.saturating_sub(*e))
                    .map_or(rest.len(), |(i, _)| i);
            &s.as_bytes()[start..end.max(start)]
        }
        _ => panic!("expected a String.Slice, got {a:?}"),
    }
}

/// A position as the `u64` the crate takes: a `Nat` that does not fit is past
/// the end, passed as `u64::MAX`.
fn pos_sat(a: &Arg) -> u64 {
    u64::try_from(nat(a)).unwrap_or(u64::MAX)
}

// ------------------------------------------------------------------ the functions

/// What a call printed and returned: Lean's `repr` of the result, the panic
/// message it reported, and the bits of a `Float`/`Float32` result.
#[derive(Default)]
struct Out {
    value: String,
    panic: Option<String>,
    bits: Option<String>,
}

impl From<String> for Out {
    fn from(value: String) -> Out {
        Out {
            value,
            ..Out::default()
        }
    }
}

/// A `Float` result: its `repr` (`Float.toString` at precedence 0) and bits.
fn fret(x: f64) -> Out {
    Out {
        value: f64_str(x),
        bits: Some(format!("0x{:016x}", float::to_bits(x))),
        panic: None,
    }
}

/// A `Float32` result.
fn gret(x: f32) -> Out {
    Out {
        value: f32_str(x),
        bits: Some(format!("0x{:08x}", float32::to_bits(x))),
        panic: None,
    }
}

type Eval = Box<dyn Fn(&[Arg]) -> Out>;

struct Registry(HashMap<String, Eval>);

impl Registry {
    fn add<O: Into<Out>>(&mut self, name: &str, f: impl Fn(&[Arg]) -> O + 'static) {
        let prev = self
            .0
            .insert(name.to_string(), Box::new(move |a| f(a).into()));
        assert!(prev.is_none(), "{name} registered twice");
    }
}

fn registry() -> Registry {
    let mut r = Registry(HashMap::new());
    hash_fns(&mut r);
    float_fns(&mut r);
    libm_fns(&mut r);
    uint_fns(&mut r);
    sint_fns(&mut r);
    string_fns(&mut r);
    net_fns(&mut r);
    toolchain_fns(&mut r);
    r
}

fn hash_fns(r: &mut Registry) {
    r.add("mixHash", |a| {
        hash::uint64_mix_hash(nat_low64(&a[0]), nat_low64(&a[1])).to_string()
    });
    r.add("String.hash", |a| {
        hash::string_hash(bytes(&a[0])).to_string()
    });
    r.add("ByteArray.hash", |a| {
        hash::byte_array_hash(bytes(&a[0])).to_string()
    });
    r.add("String.Slice.hash", |a| {
        hash::slice_hash(slice(&a[0])).to_string()
    });
}

fn float_fns(r: &mut Registry) {
    r.add("Float.toString", |a| {
        let mut s = String::new();
        float::to_string(f64a(&a[0]), &mut s).unwrap();
        str_repr(s.as_bytes())
    });
    r.add("Float32.toString", |a| {
        let mut s = String::new();
        float32::to_string(f32a(&a[0]), &mut s).unwrap();
        str_repr(s.as_bytes())
    });
    macro_rules! conv {
        ($($lean:literal => $f:path, $arg:ident, $show:expr;)*) => {
            $( r.add($lean, |a| { let v = $f($arg(&a[0])); ($show)(v) }); )*
        };
    }
    conv! {
        "Float.toUInt8" => float::to_uint8, f64a, |v: u8| v.to_string();
        "Float.toUInt16" => float::to_uint16, f64a, |v: u16| v.to_string();
        "Float.toUInt32" => float::to_uint32, f64a, |v: u32| v.to_string();
        "Float.toUInt64" => float::to_uint64, f64a, |v: u64| v.to_string();
        "Float.toUSize" => float::to_usize, f64a, |v: usize| v.to_string();
        "Float.toInt8" => float::to_int8, f64a, |v: u8| (v as i8).to_string();
        "Float.toInt16" => float::to_int16, f64a, |v: u16| (v as i16).to_string();
        "Float.toInt32" => float::to_int32, f64a, |v: u32| (v as i32).to_string();
        "Float.toInt64" => float::to_int64, f64a, |v: u64| (v as i64).to_string();
        "Float.toISize" => float::to_isize, f64a, |v: usize| (v as isize).to_string();
        "Float32.toUInt8" => float32::to_uint8, f32a, |v: u8| v.to_string();
        "Float32.toUInt16" => float32::to_uint16, f32a, |v: u16| v.to_string();
        "Float32.toUInt32" => float32::to_uint32, f32a, |v: u32| v.to_string();
        "Float32.toUInt64" => float32::to_uint64, f32a, |v: u64| v.to_string();
        "Float32.toUSize" => float32::to_usize, f32a, |v: usize| v.to_string();
        "Float32.toInt8" => float32::to_int8, f32a, |v: u8| (v as i8).to_string();
        "Float32.toInt16" => float32::to_int16, f32a, |v: u16| (v as i16).to_string();
        "Float32.toInt32" => float32::to_int32, f32a, |v: u32| (v as i32).to_string();
        "Float32.toInt64" => float32::to_int64, f32a, |v: u64| (v as i64).to_string();
        "Float32.toISize" => float32::to_isize, f32a, |v: usize| (v as isize).to_string();
        "Float.toBits" => float::to_bits, f64a, |v: u64| v.to_string();
        "Float32.toBits" => float32::to_bits, f32a, |v: u32| v.to_string();
        "Float.isNaN" => float::isnan, f64a, |v: bool| v.to_string();
        "Float.isInf" => float::isinf, f64a, |v: bool| v.to_string();
        "Float.isFinite" => float::isfinite, f64a, |v: bool| v.to_string();
        "Float32.isNaN" => float32::isnan, f32a, |v: bool| v.to_string();
        "Float32.isInf" => float32::isinf, f32a, |v: bool| v.to_string();
        "Float32.isFinite" => float32::isfinite, f32a, |v: bool| v.to_string();
    }
    r.add("Float.ofBits", |a| fret(float::of_bits(nat_low64(&a[0]))));
    r.add("Float32.ofBits", |a| {
        gret(float32::of_bits(nat_low64(&a[0]) as u32))
    });
    r.add("Float.frExp", |a| {
        let (m, e) = float::frexp(f64a(&a[0]));
        let m = fret(m);
        Out {
            value: format!("({}, {e})", m.value),
            ..m
        }
    });
    r.add("Float32.frExp", |a| {
        let (m, e) = float32::frexp(f32a(&a[0]));
        let m = gret(m);
        Out {
            value: format!("({}, {e})", m.value),
            ..m
        }
    });
    r.add("Float.scaleB", |a| {
        fret(float::scaleb(f64a(&a[0]), int_sat(&a[1])))
    });
    r.add("Float32.scaleB", |a| {
        gret(float32::scaleb(f32a(&a[0]), int_sat(&a[1])))
    });
}

fn libm_fns(r: &mut Registry) {
    macro_rules! unary {
        ($($lean:literal => $f:path, $g:path;)*) => {
            $(
                r.add(concat!("Float.", $lean), |a| fret($f(f64a(&a[0]))));
                r.add(concat!("Float32.", $lean), |a| gret($g(f32a(&a[0]))));
            )*
        };
    }
    unary! {
        "abs" => libm::fabs, libm::fabsf; "acos" => libm::acos, libm::acosf;
        "acosh" => libm::acosh, libm::acoshf; "asin" => libm::asin, libm::asinf;
        "asinh" => libm::asinh, libm::asinhf; "atan" => libm::atan, libm::atanf;
        "ceil" => libm::ceil, libm::ceilf; "cos" => libm::cos, libm::cosf;
        "cosh" => libm::cosh, libm::coshf; "exp" => libm::exp, libm::expf;
        "exp2" => libm::exp2, libm::exp2f; "floor" => libm::floor, libm::floorf;
        "log" => libm::log, libm::logf; "log10" => libm::log10, libm::log10f;
        "log2" => libm::log2, libm::log2f; "round" => libm::round, libm::roundf;
        "sin" => libm::sin, libm::sinf; "sinh" => libm::sinh, libm::sinhf;
        "sqrt" => libm::sqrt, libm::sqrtf; "tan" => libm::tan, libm::tanf;
        "tanh" => libm::tanh, libm::tanhf;
    }
    r.add("Float.cbrt", |a| fret(libm::cbrt(f64a(&a[0]))));
    r.add("Float32.cbrt", |a| gret(libm::cbrtf(f32a(&a[0]))));
    r.add("Float.atanh", |a| fret(libm::atanh(f64a(&a[0]))));
    r.add("Float32.atanh", |a| gret(libm::atanhf(f32a(&a[0]))));
    r.add("Float.atan2", |a| {
        fret(libm::atan2(f64a(&a[0]), f64a(&a[1])))
    });
    r.add("Float32.atan2", |a| {
        gret(libm::atan2f(f32a(&a[0]), f32a(&a[1])))
    });
    r.add("Float.pow", |a| fret(libm::pow(f64a(&a[0]), f64a(&a[1]))));
    r.add("Float32.pow", |a| {
        gret(libm::powf(f32a(&a[0]), f32a(&a[1])))
    });
}

fn uint_fns(r: &mut Registry) {
    macro_rules! uint {
        ($($lean:literal: $t:ty => $div:ident $rem:ident $shl:ident $shr:ident $log2:ident
            $of_nat:ident $to_nat:ident;)*) => {
            $(
                r.add(concat!($lean, ".div"), |a| {
                    uint::$div(nat_low64(&a[0]) as $t, nat_low64(&a[1]) as $t).to_string()
                });
                r.add(concat!($lean, ".mod"), |a| {
                    uint::$rem(nat_low64(&a[0]) as $t, nat_low64(&a[1]) as $t).to_string()
                });
                r.add(concat!($lean, ".shiftLeft"), |a| {
                    uint::$shl(nat_low64(&a[0]) as $t, nat_low64(&a[1]) as $t).to_string()
                });
                r.add(concat!($lean, ".shiftRight"), |a| {
                    uint::$shr(nat_low64(&a[0]) as $t, nat_low64(&a[1]) as $t).to_string()
                });
                r.add(concat!($lean, ".log2"), |a| uint::$log2(nat_low64(&a[0]) as $t).to_string());
                r.add(concat!($lean, ".ofNat"), |a| uint::$of_nat(nat_low64(&a[0])).to_string());
                r.add(concat!($lean, ".toNat"), |a| {
                    uint::$to_nat(nat_low64(&a[0]) as $t).to_string()
                });
            )*
        };
    }
    uint! {
        "UInt8": u8 => uint8_div uint8_mod uint8_shift_left uint8_shift_right uint8_log2
            uint8_of_nat uint8_to_nat;
        "UInt16": u16 => uint16_div uint16_mod uint16_shift_left uint16_shift_right uint16_log2
            uint16_of_nat uint16_to_nat;
        "UInt32": u32 => uint32_div uint32_mod uint32_shift_left uint32_shift_right uint32_log2
            uint32_of_nat uint32_to_nat;
        "UInt64": u64 => uint64_div uint64_mod uint64_shift_left uint64_shift_right uint64_log2
            uint64_of_nat uint64_to_nat;
        "USize": usize => usize_div usize_mod usize_shift_left usize_shift_right usize_log2
            usize_of_nat usize_to_nat;
    }
}

fn sint_fns(r: &mut Registry) {
    macro_rules! sint {
        ($($lean:literal: $u:ty as $s:ty => $div:ident $rem:ident $shl:ident $shr:ident $abs:ident
            $lt:ident $le:ident $to_int:ident $of_int:ident $of_nat:ident $to_float:ident
            $to_float32:ident;)*) => {
            $(
                r.add(concat!($lean, ".div"), |a| {
                    (sint::$div(int_low64(&a[0]) as $u, int_low64(&a[1]) as $u) as $s).to_string()
                });
                r.add(concat!($lean, ".mod"), |a| {
                    (sint::$rem(int_low64(&a[0]) as $u, int_low64(&a[1]) as $u) as $s).to_string()
                });
                r.add(concat!($lean, ".shiftLeft"), |a| {
                    (sint::$shl(int_low64(&a[0]) as $u, int_low64(&a[1]) as $u) as $s).to_string()
                });
                r.add(concat!($lean, ".shiftRight"), |a| {
                    (sint::$shr(int_low64(&a[0]) as $u, int_low64(&a[1]) as $u) as $s).to_string()
                });
                r.add(concat!($lean, ".abs"), |a| {
                    (sint::$abs(int_low64(&a[0]) as $u) as $s).to_string()
                });
                r.add(concat!($lean, ".decLt"), |a| {
                    sint::$lt(int_low64(&a[0]) as $u, int_low64(&a[1]) as $u).to_string()
                });
                r.add(concat!($lean, ".decLe"), |a| {
                    sint::$le(int_low64(&a[0]) as $u, int_low64(&a[1]) as $u).to_string()
                });
                r.add(concat!($lean, ".toInt"), |a| sint::$to_int(int_low64(&a[0]) as $u).to_string());
                r.add(concat!($lean, ".ofInt"), |a| {
                    (sint::$of_int(int_low64(&a[0])) as $s).to_string()
                });
                r.add(concat!($lean, ".ofNat"), |a| {
                    (sint::$of_nat(nat_low64(&a[0])) as $s).to_string()
                });
                r.add(concat!($lean, ".toFloat"), |a| {
                    fret(sint::$to_float(int_low64(&a[0]) as $u))
                });
                r.add(concat!($lean, ".toFloat32"), |a| {
                    gret(sint::$to_float32(int_low64(&a[0]) as $u))
                });
            )*
        };
    }
    sint! {
        "Int8": u8 as i8 => int8_div int8_mod int8_shift_left int8_shift_right int8_abs
            int8_dec_lt int8_dec_le int8_to_int int8_of_int int8_of_nat int8_to_float
            int8_to_float32;
        "Int16": u16 as i16 => int16_div int16_mod int16_shift_left int16_shift_right int16_abs
            int16_dec_lt int16_dec_le int16_to_int int16_of_int int16_of_nat int16_to_float
            int16_to_float32;
        "Int32": u32 as i32 => int32_div int32_mod int32_shift_left int32_shift_right int32_abs
            int32_dec_lt int32_dec_le int32_to_int int32_of_int int32_of_nat int32_to_float
            int32_to_float32;
        "Int64": u64 as i64 => int64_div int64_mod int64_shift_left int64_shift_right int64_abs
            int64_dec_lt int64_dec_le int64_to_int_sint int64_of_int int64_of_nat int64_to_float
            int64_to_float32;
        "ISize": usize as isize => isize_div isize_mod isize_shift_left isize_shift_right isize_abs
            isize_dec_lt isize_dec_le isize_to_int isize_of_int isize_of_nat isize_to_float
            isize_to_float32;
    }
    macro_rules! convert {
        ($($lean:literal => $f:ident: $u:ty => $ts:ty;)*) => {
            $( r.add($lean, |a| (sint::$f(int_low64(&a[0]) as $u) as $ts).to_string()); )*
        };
    }
    convert! {
        "Int8.toInt16" => int8_to_int16: u8 => i16;
        "Int8.toInt32" => int8_to_int32: u8 => i32;
        "Int8.toInt64" => int8_to_int64: u8 => i64;
        "Int8.toISize" => int8_to_isize: u8 => isize;
        "Int16.toInt8" => int16_to_int8: u16 => i8;
        "Int16.toInt32" => int16_to_int32: u16 => i32;
        "Int16.toInt64" => int16_to_int64: u16 => i64;
        "Int16.toISize" => int16_to_isize: u16 => isize;
        "Int32.toInt8" => int32_to_int8: u32 => i8;
        "Int32.toInt16" => int32_to_int16: u32 => i16;
        "Int32.toInt64" => int32_to_int64: u32 => i64;
        "Int32.toISize" => int32_to_isize: u32 => isize;
        "Int64.toInt8" => int64_to_int8: u64 => i8;
        "Int64.toInt16" => int64_to_int16: u64 => i16;
        "Int64.toInt32" => int64_to_int32: u64 => i32;
        "Int64.toISize" => int64_to_isize: u64 => isize;
        "ISize.toInt8" => isize_to_int8: usize => i8;
        "ISize.toInt16" => isize_to_int16: usize => i16;
        "ISize.toInt32" => isize_to_int32: usize => i32;
        "ISize.toInt64" => isize_to_int64: usize => i64;
    }
}

fn string_fns(r: &mut Registry) {
    r.add("String.Pos.Raw.get", |a| {
        char_repr(string::utf8_get(bytes(&a[0]), pos_sat(&a[1])))
    });
    r.add("String.Pos.Raw.get?", |a| {
        match string::utf8_get_opt(bytes(&a[0]), pos_sat(&a[1])) {
            Some(c) => format!("some {}", char_repr(c)),
            None => "none".to_string(),
        }
    });
    r.add("String.Pos.Raw.get!", |a| {
        match string::utf8_get_bang(bytes(&a[0]), pos_sat(&a[1])) {
            Some(c) => Out::from(char_repr(c)),
            None => Out {
                panic: Some(string::GET_BANG_PANIC.to_string()),
                value: char_repr(string::CHAR_DEFAULT),
                bits: None,
            },
        }
    });
    r.add("String.Pos.Raw.get'", |a| {
        char_repr(string::utf8_get_fast(bytes(&a[0]), pos_sat(&a[1])))
    });
    r.add("String.decodeChar", |a| {
        char_repr(string::utf8_get_fast(bytes(&a[0]), pos_sat(&a[1])))
    });
    // A position at or above 2^63 (a big `Nat` in Lean's C) gets `p + 1` /
    // `p - 1` from the caller's own `Nat` arithmetic, as the crate documents.
    r.add("String.Pos.Raw.next", |a| {
        let p = nat(&a[1]);
        if p < 1 << 63 {
            pos_repr(u128::from(string::utf8_next(bytes(&a[0]), p as u64)))
        } else {
            pos_repr(p + 1)
        }
    });
    r.add("String.Pos.Raw.next'", |a| {
        pos_repr(u128::from(string::utf8_next_fast(
            bytes(&a[0]),
            pos_sat(&a[1]),
        )))
    });
    r.add("String.Pos.Raw.prev", |a| {
        let p = nat(&a[1]);
        if p < 1 << 63 {
            pos_repr(u128::from(string::utf8_prev(bytes(&a[0]), p as u64)))
        } else {
            pos_repr(p - 1)
        }
    });
    r.add("String.Pos.Raw.atEnd", |a| {
        string::utf8_at_end(bytes(&a[0]), pos_sat(&a[1])).to_string()
    });
    r.add("String.Pos.Raw.isValid", |a| {
        string::is_valid_pos(bytes(&a[0]), pos_sat(&a[1])).to_string()
    });
    r.add("String.Pos.Raw.extract", |a| {
        let s = bytes(&a[0]);
        str_repr(&s[string::utf8_extract(s, pos_sat(&a[1]), pos_sat(&a[2]))])
    });
    r.add("String.extract", |a| {
        let s = bytes(&a[0]);
        str_repr(&s[string::utf8_extract_fast(s, pos_sat(&a[1]), pos_sat(&a[2]))])
    });
    r.add("String.getUTF8Byte", |a| {
        string::get_byte_fast(bytes(&a[0]), pos_sat(&a[1])).to_string()
    });
    r.add("String.Internal.ugetUTF8Byte", |a| {
        string::get_byte_fast(bytes(&a[0]), pos_sat(&a[1])).to_string()
    });
    // `String.length` is the count cached when the string is made: the count
    // `utf8_strlen` gives (and its `const` twin, for literals).
    r.add("String.length", |a| {
        let n = string::utf8_strlen(bytes(&a[0]));
        assert_eq!(n, string::utf8_strlen_const(bytes(&a[0])));
        n.to_string()
    });
    r.add("String.Slice.Pattern.Internal.memcmpStr", |a| {
        string::memcmp(
            bytes(&a[0]),
            bytes(&a[1]),
            pos_sat(&a[2]),
            pos_sat(&a[3]),
            pos_sat(&a[4]),
        )
        .to_string()
    });
    r.add("String.decidableLT", |a| {
        string::lt(bytes(&a[0]), bytes(&a[1])).to_string()
    });
    r.add("String.compare", |a| {
        ordering_repr(string::compare(bytes(&a[0]), bytes(&a[1])))
    });
    r.add("String.Slice.instDecidableLt", |a| {
        string::lt(slice(&a[0]), slice(&a[1])).to_string()
    });
    // `String.Pos.set`'s position is valid by proof; all three are one extern.
    for name in ["String.Pos.Raw.set", "String.Pos.set", "String.set"] {
        r.add(name, |a| {
            let s = bytes(&a[0]);
            match string::utf8_set(s, pos_sat(&a[1]), chr(&a[2])) {
                None => str_repr(s),
                // a unique string's path
                Some(change) if change.same_size() => {
                    let mut v = s.to_vec();
                    change.write_in_place(&mut v);
                    str_repr(&v)
                }
                Some(change) => {
                    let mut v = Vec::with_capacity(change.result_size(s.len()));
                    v.extend_from_slice(&s[..change.start]);
                    v.extend_from_slice(change.new_bytes());
                    v.extend_from_slice(&s[change.end..]);
                    str_repr(&v)
                }
            }
        });
    }
    r.add("ByteArray.validateUTF8", |a| {
        string::validate_utf8(bytes(&a[0])).to_string()
    });
}

/// An `Array UInt8`/`Array UInt16`'s `repr`: `#[1, 2]`.
fn array_repr<T: ToString>(xs: &[T]) -> String {
    let items: Vec<String> = xs.iter().map(T::to_string).collect();
    format!("#[{}]", items.join(", "))
}

fn option_repr(x: Option<String>) -> String {
    match x {
        Some(v) => format!("some {v}"),
        None => "none".to_string(),
    }
}

fn net_fns(r: &mut Registry) {
    r.add(
        "fun s => (Std.Net.IPv4Addr.ofString s).map (·.octets.toArray)",
        |a| option_repr(net::pton_v4(bytes(&a[0])).map(|o| array_repr(&o))),
    );
    r.add(
        "fun s => (Std.Net.IPv6Addr.ofString s).map (·.segments.toArray)",
        |a| option_repr(net::pton_v6(bytes(&a[0])).map(|w| array_repr(&w))),
    );
    r.add(
        "fun a b c d => (Std.Net.IPv4Addr.ofParts a b c d).toString",
        |a| {
            let o: [u8; 4] = std::array::from_fn(|k| nat_low64(&a[k]) as u8);
            let mut s = String::new();
            net::ntop_v4(o, &mut s).unwrap();
            str_repr(s.as_bytes())
        },
    );
    r.add(
        "fun a b c d e f g h => (Std.Net.IPv6Addr.ofParts a b c d e f g h).toString",
        |a| {
            let w: [u16; 8] = std::array::from_fn(|k| nat_low64(&a[k]) as u16);
            let mut s = String::new();
            net::ntop_v6(w, &mut s).unwrap();
            str_repr(s.as_bytes())
        },
    );
}

fn toolchain_fns(r: &mut Registry) {
    r.add("Lean.getGithash", |_| {
        str_repr(toolchain::GITHASH.as_bytes())
    });
    r.add("System.Platform.getTarget", |_| {
        str_repr(toolchain::PLATFORM_TARGET.as_bytes())
    });
    r.add("Lean.version.getSpecialDesc", |_| {
        str_repr(toolchain::SPECIAL_DESC.as_bytes())
    });
}

// ------------------------------------------------------------------ the tests

/// Whether the call's output is the row's: `expected` (or, for a panic,
/// `panic: <message>`, the returned `default` and the `stderr` text), and the
/// bits of a float result.
fn mismatch(row: &Row, out: &Out) -> Option<String> {
    let want = match &out.panic {
        None => (out.value.clone(), None, None),
        Some(msg) => (
            format!("panic: {msg}"),
            Some(out.value.clone()),
            Some(format!("{msg}\n")),
        ),
    };
    let have = (
        row.expected.clone(),
        row.default.clone(),
        row.stderr.clone(),
    );
    let bits_ok = row.result_bits.is_none() || row.result_bits == out.bits;
    if want == have && bits_ok {
        None
    } else {
        Some(format!(
            "    expected {have:?} bits {:?}\n    got      {want:?} bits {:?}",
            row.result_bits, out.bits
        ))
    }
}

/// The rows are compiled in (`include_str!`), so the tests read no files and
/// also run under Miri.
fn run(file: &str, text: &str) {
    let rows = read_rows(file, text);
    assert!(!rows.is_empty(), "{file}: no rows");
    let reg = registry();
    let mut failures = Vec::new();
    for row in &rows {
        let Some(f) = reg.0.get(&row.func) else {
            failures.push(format!("{}: no glue for {}", row.id, row.func));
            continue;
        };
        if let Some(m) = mismatch(row, &f(&row.args)) {
            failures.push(format!("{}: {} {:?}\n{m}", row.id, row.func, row.args));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} rows fail:\n{}",
        failures.len(),
        rows.len(),
        failures.join("\n")
    );
}

#[test]
fn hash_rows() {
    run("hash", include_str!("cases/hash/hash.rows.toml"));
}

#[test]
fn float_rows() {
    run("float", include_str!("cases/float/float.rows.toml"));
}

/// glibc's libm is foreign code, which Miri does not run.
#[test]
#[cfg_attr(miri, ignore)]
fn libm_rows() {
    run("libm", include_str!("cases/libm/libm.rows.toml"));
}

#[test]
fn uint_rows() {
    run("uint", include_str!("cases/uint/uint.rows.toml"));
}

#[test]
fn sint_rows() {
    run("sint", include_str!("cases/sint/sint.rows.toml"));
}

#[test]
fn string_rows() {
    run("string", include_str!("cases/string/string.rows.toml"));
}

#[test]
fn net_rows() {
    run("net", include_str!("cases/net/net.rows.toml"));
}

/// The toolchain's facts as native Lean 4.34.0 reports them on the pinned host
/// (`semantics::toolchain`).
#[test]
fn toolchain_rows() {
    run(
        "toolchain",
        include_str!("cases/toolchain/toolchain.rows.toml"),
    );
}
