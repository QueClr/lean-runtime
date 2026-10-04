//! Runs every row of batch 2's areas (`nat`, `int`, `array`, `panic`, `repr`;
//! expected values from native Lean 4.34.0, see `tests/cases/README.md`)
//! against the crate, with the plain backend of `common::refbig`.
//!
//! Each Lean function maps to the crate's function plus the glue a translator
//! writes around it: `Nat`/`Int` arguments made into `Nat<RNat>`/`Int<RInt>`,
//! an internal panic turned into the process's end, a panic plan into the
//! message and the default, an array plan carried out on a `Vec`, and the
//! result rendered as Lean's `repr`.
//!
//! Every `Nat`/`Int` row runs under four representations: the translators'
//! (`Small` below 2^63 for `Nat`; `Small` in the `int32` range for `Int`, as
//! lean2rr and Lean's C), leanrs's `Int` (`Small` in the `i64` range),
//! every argument `Big`, and every value below 2^64 `Small` (so words of
//! 2^63 and more, and mixed word and big pairs), so that each rule's word
//! path and each slow path meets every value.
//!
//! `tests/rows.rs` runs the other areas; the two share the row reader in
//! `common/`. This runner takes arguments of any size (`refbig`), runs rows
//! that end the process as data (the internal panic or abort they print),
//! and runs each `Nat`/`Int` row under four representations.

mod common;
mod refbig;

use std::collections::HashMap;
use std::fmt::Write as _;

use common::{Toml, Value};
use lean_runtime::semantics::bignum::{BigInt, BigNat};
use lean_runtime::semantics::int::{self, Int};
use lean_runtime::semantics::nat::{self, Nat};
use lean_runtime::semantics::panic::{self, InternalPanic, PanicEnd, PanicSettings};
use lean_runtime::semantics::{array, repr};
use refbig::{RInt, RNat};

// ------------------------------------------------------------------ rows

#[derive(Clone, Debug)]
enum Arg {
    Nat(RNat),
    Int(RInt),
    Str(String),
    Bytes(Vec<u8>),
    F64(f64),
    Char(char),
    Bool(bool),
    Array(Vec<u64>),
    Floats(Vec<f64>),
}

struct Row {
    id: String,
    func: String,
    args: Vec<Arg>,
    /// An argument of 2^20 bits or more (skipped under Miri).
    huge: bool,
    env: Vec<(String, String)>,
    expected: String,
    default: Option<String>,
    stderr: Option<String>,
    ends: Option<(String, i64)>,
    result_bits: Option<String>,
    deviation: bool,
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
    Err("unterminated literal".into())
}

fn is_float_literal(t: &str) -> bool {
    let t = t
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim_start_matches('-');
    t.contains('.') && t.parse::<f64>().is_ok()
}

fn parse_nat_text(t: &str) -> Result<RNat, String> {
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("not a numeral: {t}"));
    }
    Ok(RNat::from_decimal(t))
}

fn parse_arg(term: &str, float_bits: &mut std::vec::IntoIter<String>) -> Result<Arg, String> {
    let t = term.trim();
    if is_float_literal(t) {
        let b = float_bits.next().ok_or(format!("no bits for {t}"))?;
        let hex = b.strip_prefix("0x").ok_or("bits without 0x")?;
        let n = u64::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
        return Ok(Arg::F64(f64::from_bits(n)));
    }
    if t.starts_with('"') {
        let (s, n) = parse_literal(t, '"')?;
        assert_eq!(n, t.len(), "trailing text in {t}");
        return Ok(Arg::Str(s));
    }
    if t.starts_with('\'') {
        let (s, n) = parse_literal(t, '\'')?;
        assert_eq!(n, t.len(), "trailing text in {t}");
        let mut cs = s.chars();
        let c = cs.next().ok_or("empty character literal")?;
        assert!(cs.next().is_none(), "one character: {t}");
        return Ok(Arg::Char(c));
    }
    match t {
        "true" => return Ok(Arg::Bool(true)),
        "false" => return Ok(Arg::Bool(false)),
        _ => {}
    }
    if let Some(inner) = t.strip_prefix("#[").and_then(|t| t.strip_suffix(']')) {
        let xs = inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
            .collect::<Result<Vec<u64>, _>>()?;
        return Ok(Arg::Array(xs));
    }
    if let Some(inner) = t
        .strip_prefix("(ByteArray.mk #[")
        .and_then(|t| t.strip_suffix("])"))
    {
        let bs = inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<u8>().map_err(|e| e.to_string()))
            .collect::<Result<Vec<u8>, _>>()?;
        return Ok(Arg::Bytes(bs));
    }
    if let Some(inner) = t
        .strip_prefix("(FloatArray.mk #[")
        .and_then(|t| t.strip_suffix("])"))
    {
        // decimal literals that are exact in binary, as gen_rows.py sends them
        let xs = inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<f64>().map_err(|e| e.to_string()))
            .collect::<Result<Vec<f64>, _>>()?;
        return Ok(Arg::Floats(xs));
    }
    if let Some(inner) = t.strip_prefix("(2 ^ ").and_then(|t| t.strip_suffix(')')) {
        // `(2 ^ K)` or `(2 ^ K + A)`
        let (k, a) = match inner.split_once(" + ") {
            Some((k, a)) => (k, a.parse::<u64>().map_err(|e| e.to_string())?),
            None => (inner, 0),
        };
        let k: u64 = k.parse().map_err(|_| format!("bad power {t}"))?;
        let v = if a == 0 {
            RNat::pow2(k)
        } else {
            RNat::pow2_plus(k, a)
        };
        return Ok(Arg::Nat(v));
    }
    if let Some(inner) = t.strip_prefix("(-").and_then(|t| t.strip_suffix(')')) {
        let m = parse_nat_text(inner)?;
        return Ok(Arg::Int(RInt::from_nat(m).neg()));
    }
    Ok(Arg::Nat(parse_nat_text(t)?))
}

/// The power `K` of an argument written `(2 ^ K ...)`.
fn power_of(term: &str) -> Option<u64> {
    let rest = term.trim().strip_prefix("(2 ^ ")?;
    rest.split([' ', ')']).next()?.parse().ok()
}

/// The rows of a file. Under Miri the rows with an argument of 2^20 bits or
/// more get no arguments (`run` skips them).
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
            let texts = t.get("args").map(Value::strings).unwrap_or_default();
            // a `fun` row's result is 2^32 bits or more, as are `(2 ^ K)` arguments
            let huge = func.starts_with("fun ")
                || texts
                    .iter()
                    .any(|a| power_of(a).is_some_and(|k| k >= 1 << 20));
            let args = if cfg!(miri) && huge {
                Vec::new()
            } else {
                let args = texts
                    .iter()
                    .map(|a| parse_arg(a, &mut float_bits))
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap_or_else(|e| panic!("{file}: {id}: {e}"));
                assert!(float_bits.next().is_none(), "{id}: unused bits.args");
                args
            };
            let env = t
                .get("env")
                .map(|e| {
                    e.entries()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let ends = t.get("ends").map(|e| {
                (
                    e.get("stderr").expect("ends.stderr").as_str().to_string(),
                    e.get("code").expect("ends.code").as_int(),
                )
            });
            // An `LB-nn` deviation (the crate's own) expects the definition's
            // result and records native's in `native`; a one-translator
            // deviation (leanrs's `DVn`) expects native's.
            let deviation = t.get("native").is_some();
            let lb = t.get("deviations").is_some_and(|d| {
                d.entries()
                    .iter()
                    .any(|(_, v)| v.as_str().starts_with("LB-"))
            });
            assert_eq!(
                deviation, lb,
                "{id}: an LB deviation records native's outcome"
            );
            Row {
                func,
                args,
                huge,
                env,
                expected: field("expected").expect("a row has an expected value"),
                default: field("default"),
                stderr: field("stderr"),
                ends,
                result_bits: bits
                    .and_then(|b| b.get("result"))
                    .map(|v| v.as_str().to_string()),
                deviation,
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

/// `String.quote`, written independently of `semantics::repr` (batch 1's
/// test helper), since the rows check that module.
fn str_repr(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        quote_core(&mut out, c, true);
    }
    out.push('"');
    out
}

/// `Float.toString` (glibc's `%f`, `NaN`, `inf`): Rust's `{:.6}` gives the
/// same text (semantics-1's finding), and these rows only hold exact values.
fn f64_str(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        format!("{x:.6}")
    }
}

fn nat_text(n: &Nat<RNat>) -> String {
    let mut s = String::new();
    nat::write_decimal(n, &mut s).unwrap();
    s
}

fn int_text(i: &Int<RInt>) -> String {
    let mut s = String::new();
    int::write_decimal(i, &mut s).unwrap();
    s
}

fn list_repr<T>(xs: &[T], show: impl Fn(&T) -> String) -> String {
    format!("[{}]", xs.iter().map(show).collect::<Vec<_>>().join(", "))
}

fn array_repr(xs: &[u64]) -> String {
    format!("#{}", list_repr(xs, |x| x.to_string()))
}

// ------------------------------------------------------------------ argument glue

/// Which representation the glue gives `Nat`/`Int` arguments.
#[derive(Clone, Copy, Debug)]
enum Repr {
    /// Lean's C and lean2rr: `Nat` words below 2^63, `Int` words in `int32`.
    Lean,
    /// leanrs: `Nat` words below 2^63, `Int` words in `i64`.
    Leanrs,
    /// Every argument a big number.
    AllBig,
    /// Every value that fits a `u64` a word, 2^63..2^64 included (`Int`: the
    /// `i64` range), so a word above Lean's small range meets the rules,
    /// alone and against big operands (review RS2-05).
    Words,
}

const REPRS: [Repr; 4] = [Repr::Lean, Repr::Leanrs, Repr::AllBig, Repr::Words];

fn nat_value(a: &Arg) -> RNat {
    match a {
        Arg::Nat(n) => n.clone(),
        _ => panic!("expected a Nat, got {a:?}"),
    }
}

fn int_value(a: &Arg) -> RInt {
    match a {
        Arg::Nat(n) => RInt::from_nat(n.clone()),
        Arg::Int(i) => i.clone(),
        _ => panic!("expected an Int, got {a:?}"),
    }
}

fn nat(a: &Arg, r: Repr) -> Nat<RNat> {
    let n = nat_value(a);
    match (r, n.to_u64()) {
        (Repr::Lean | Repr::Leanrs, Some(v)) if v >> 63 == 0 => Nat::Small(v),
        (Repr::Words, Some(v)) => Nat::Small(v),
        _ => Nat::Big(n),
    }
}

fn int(a: &Arg, r: Repr) -> Int<RInt> {
    let i = int_value(a);
    match (r, i.to_i64()) {
        (Repr::Lean, Some(v)) if i32::try_from(v).is_ok() => Int::Small(v),
        (Repr::Leanrs | Repr::Words, Some(v)) => Int::Small(v),
        _ => Int::Big(i),
    }
}

/// A size or index argument as the `u64` the array rules take: `u64::MAX`
/// for 2^64 or more.
fn sat(a: &Arg) -> u64 {
    nat(a, Repr::Lean).to_u64_saturating()
}

fn string(a: &Arg) -> &str {
    match a {
        Arg::Str(s) => s,
        _ => panic!("expected a String, got {a:?}"),
    }
}

fn bytes(a: &Arg) -> &[u8] {
    match a {
        Arg::Bytes(b) => b,
        _ => panic!("expected a ByteArray, got {a:?}"),
    }
}

fn floats(a: &Arg) -> &[f64] {
    match a {
        Arg::Floats(f) => f,
        _ => panic!("expected a FloatArray, got {a:?}"),
    }
}

fn array_arg(a: &Arg) -> &[u64] {
    match a {
        Arg::Array(xs) => xs,
        _ => panic!("expected an Array Nat, got {a:?}"),
    }
}

fn f64a(a: &Arg) -> f64 {
    match a {
        Arg::F64(x) => *x,
        _ => panic!("expected a Float, got {a:?}"),
    }
}

fn chr(a: &Arg) -> char {
    match a {
        Arg::Char(c) => *c,
        _ => panic!("expected a Char, got {a:?}"),
    }
}

fn boolean(a: &Arg) -> bool {
    match a {
        Arg::Bool(b) => *b,
        _ => panic!("expected a Bool, got {a:?}"),
    }
}

// ------------------------------------------------------------------ the functions

/// What a call printed and how it ended: Lean's `repr` of the result and the
/// panic message it reported, or the stderr text and status of the process's
/// end, and the bits of a `Float` result.
#[derive(Default, Debug)]
struct Out {
    value: String,
    panic: Option<String>,
    ends: Option<(String, i64)>,
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

/// The settings the oracle's process ran with (`LEAN_BACKTRACE=0` and the
/// row's `env`).
fn settings(env: &[(String, String)]) -> PanicSettings {
    let abort = env
        .iter()
        .find(|(k, _)| k == "LEAN_ABORT_ON_PANIC")
        .map(|(_, v)| v.as_bytes());
    PanicSettings::from_env(abort, Some(b"0"))
}

/// The end of the process after an internal panic: its line and status.
fn internal_end(p: InternalPanic, env: &[(String, String)]) -> Out {
    let mut line = String::new();
    p.write_line(&mut line).unwrap();
    let end = panic::internal_panic_end(settings(env));
    Out {
        ends: Some((
            line,
            panic::end_status(end).expect("an internal panic ends") as i64,
        )),
        ..Out::default()
    }
}

/// A `lean_panic_fn` call with `msg`, then `value` if the process goes on.
fn panic_fn(msg: &str, value: String, env: &[(String, String)]) -> Out {
    let plan = panic::panic_fn_plan(settings(env));
    assert!(
        plan.print && !plan.backtrace,
        "the oracle runs with messages and no backtrace"
    );
    match plan.end {
        PanicEnd::Return => Out {
            value,
            panic: Some(msg.to_string()),
            ..Out::default()
        },
        end => Out {
            ends: Some((
                format!("{msg}\n"),
                panic::end_status(end).expect("an abort or exit ends") as i64,
            )),
            ..Out::default()
        },
    }
}

/// The result of a rule that may end the process.
fn ending(r: Result<String, InternalPanic>, env: &[(String, String)]) -> Out {
    match r {
        Ok(v) => Out::from(v),
        Err(p) => internal_end(p, env),
    }
}

type Eval = Box<dyn Fn(&[Arg], Repr, &[(String, String)]) -> Out>;

struct Registry(HashMap<String, Eval>);

impl Registry {
    fn add<O: Into<Out>>(
        &mut self,
        name: &str,
        f: impl Fn(&[Arg], Repr, &[(String, String)]) -> O + 'static,
    ) {
        let prev = self
            .0
            .insert(name.to_string(), Box::new(move |a, r, e| f(a, r, e).into()));
        assert!(prev.is_none(), "{name} registered twice");
    }
}

fn registry() -> Registry {
    let mut r = Registry(HashMap::new());
    nat_fns(&mut r);
    int_fns(&mut r);
    array_fns(&mut r);
    panic_fns(&mut r);
    repr_fns(&mut r);
    r
}

fn nat_fns(r: &mut Registry) {
    macro_rules! binary {
        ($($lean:literal => $f:path;)*) => {
            $( r.add($lean, |a, rp, _| nat_text(&$f(nat(&a[0], rp), nat(&a[1], rp)))); )*
        };
    }
    macro_rules! sized {
        ($($lean:literal => $f:path;)*) => {
            $( r.add($lean, |a, rp, env| {
                ending($f(nat(&a[0], rp), nat(&a[1], rp)).map(|v| nat_text(&v)), env)
            }); )*
        };
    }
    sized! { "Nat.add" => nat::add; "Nat.mul" => nat::mul; }
    binary! {
        "Nat.sub" => nat::sub;
        "Nat.div" => nat::div; "Nat.mod" => nat::rem; "Nat.divExact" => nat::div_exact;
        "Nat.gcd" => nat::gcd; "Nat.land" => nat::land; "Nat.lor" => nat::lor;
        "Nat.xor" => nat::lxor; "Nat.shiftRight" => nat::shiftr;
    }
    macro_rules! test {
        ($($lean:literal => $f:path;)*) => {
            $( r.add($lean, |a, rp, _| $f(&nat(&a[0], rp), &nat(&a[1], rp)).to_string()); )*
        };
    }
    test! {
        "Nat.decEq" => nat::dec_eq; "Nat.beq" => nat::dec_eq; "Nat.decLt" => nat::dec_lt;
        "Nat.decLe" => nat::dec_le; "Nat.ble" => nat::dec_le;
    }
    r.add("Nat.pow", |a, rp, env| {
        ending(
            nat::pow(nat(&a[0], rp), nat(&a[1], rp)).map(|v| nat_text(&v)),
            env,
        )
    });
    r.add("Nat.shiftLeft", |a, rp, env| {
        ending(
            nat::shiftl(nat(&a[0], rp), nat(&a[1], rp)).map(|v| nat_text(&v)),
            env,
        )
    });
    r.add("Nat.log2", |a, rp, _| {
        nat::log2(&nat(&a[0], rp)).to_string()
    });
    // results too big to print (LB-11, LB-12), seen through their log2
    r.add("fun a e => Nat.log2 (a ^ e)", |a, rp, env| {
        ending(
            nat::pow(nat(&a[0], rp), nat(&a[1], rp)).map(|v| nat::log2(&v).to_string()),
            env,
        )
    });
    r.add("fun a s => Nat.log2 (a <<< s)", |a, rp, env| {
        ending(
            nat::shiftl(nat(&a[0], rp), nat(&a[1], rp)).map(|v| nat::log2(&v).to_string()),
            env,
        )
    });
    r.add("Nat.pred", |a, rp, _| nat_text(&nat::pred(nat(&a[0], rp))));
    r.add("Nat.succ", |a, rp, env| {
        ending(nat::succ(nat(&a[0], rp)).map(|v| nat_text(&v)), env)
    });
}

fn int_fns(r: &mut Registry) {
    macro_rules! binary {
        ($($lean:literal => $f:path;)*) => {
            $( r.add($lean, |a, rp, _| int_text(&$f(int(&a[0], rp), int(&a[1], rp)))); )*
        };
    }
    macro_rules! sized {
        ($($lean:literal => $f:path;)*) => {
            $( r.add($lean, |a, rp, env| {
                ending($f(int(&a[0], rp), int(&a[1], rp)).map(|v| int_text(&v)), env)
            }); )*
        };
    }
    sized! { "Int.add" => int::add; "Int.sub" => int::sub; "Int.mul" => int::mul; }
    binary! {
        "Int.tdiv" => int::tdiv; "Int.tmod" => int::tmod; "Int.ediv" => int::ediv;
        "Int.emod" => int::emod; "Int.divExact" => int::div_exact;
    }
    macro_rules! test {
        ($($lean:literal => $f:path;)*) => {
            $( r.add($lean, |a, rp, _| $f(&int(&a[0], rp), &int(&a[1], rp)).to_string()); )*
        };
    }
    test! { "Int.decEq" => int::dec_eq; "Int.decLt" => int::dec_lt; "Int.decLe" => int::dec_le; }
    r.add("Int.decNonneg", |a, rp, _| {
        int::dec_nonneg(&int(&a[0], rp)).to_string()
    });
    r.add("Int.neg", |a, rp, _| int_text(&int::neg(int(&a[0], rp))));
    r.add("Int.natAbs", |a, rp, _| {
        nat_text(&int::nat_abs(int(&a[0], rp)))
    });
    r.add("Int.ofNat", |a, rp, _| {
        int_text(&int::of_nat::<RInt>(nat(&a[0], rp)))
    });
    r.add("Int.negSucc", |a, rp, env| {
        ending(
            int::neg_succ_of_nat::<RInt>(nat(&a[0], rp)).map(|v| int_text(&v)),
            env,
        )
    });
}

fn array_fns(r: &mut Registry) {
    r.add("Array.get!Internal", |a, _, env| {
        let xs = array_arg(&a[0]);
        match array::get_bang(xs.len(), sat(&a[1])) {
            Ok(i) => Out::from(xs[i].to_string()),
            Err(o) => panic_fn(o.message(), "0".into(), env),
        }
    });
    r.add("Array.set!", |a, _, env| {
        let mut xs = array_arg(&a[0]).to_vec();
        match array::set_bang(xs.len(), sat(&a[1])) {
            Ok(i) => {
                xs[i] = nat_value(&a[2]).to_u64().unwrap();
                Out::from(array_repr(&xs))
            }
            Err(o) => panic_fn(o.message(), array_repr(&xs), env),
        }
    });
    r.add("Array.swapIfInBounds", |a, _, _| {
        let mut xs = array_arg(&a[0]).to_vec();
        if let Some((i, j)) = array::swap_if_in_bounds(xs.len(), sat(&a[1]), sat(&a[2])) {
            xs.swap(i, j);
        }
        array_repr(&xs)
    });
    r.add("Array.pop", |a, _, _| {
        let mut xs = array_arg(&a[0]).to_vec();
        if let Some(n) = array::pop(xs.len()) {
            xs.truncate(n);
        }
        array_repr(&xs)
    });
    r.add("Array.replicate", |a, _, env| {
        let v = nat_value(&a[1]).to_u64().unwrap();
        ending(
            array::replicate_len(nat_value(&a[0]).to_u64()).map(|n| array_repr(&vec![v; n])),
            env,
        )
    });
    r.add("Array.mkEmpty", |a, _, env| {
        ending(
            array::empty_with_capacity(array::WORD_ELEMENT_BYTES, sat(&a[0])).map(|_| "#[]".into()),
            env,
        )
    });
    r.add("ByteArray.emptyWithCapacity", |a, _, env| {
        ending(
            array::empty_with_capacity(array::BYTE_ELEMENT_BYTES, sat(&a[0])).map(|_| "[]".into()),
            env,
        )
    });
    r.add("FloatArray.emptyWithCapacity", |a, _, env| {
        ending(
            array::empty_with_capacity(array::WORD_ELEMENT_BYTES, sat(&a[0])).map(|_| "[]".into()),
            env,
        )
    });
    r.add("ByteArray.get!", |a, _, _| {
        array::byte_array_get(bytes(&a[0]), sat(&a[1])).to_string()
    });
    r.add("ByteArray.set!", |a, _, _| {
        let mut b = bytes(&a[0]).to_vec();
        if let Some(i) = array::byte_array_set(b.len(), sat(&a[1])) {
            b[i] = nat_value(&a[2]).low_u64() as u8;
        }
        list_repr(&b, |x| x.to_string())
    });
    r.add("FloatArray.get!", |a, _, _| {
        let x = array::float_array_get(floats(&a[0]), sat(&a[1]));
        Out {
            value: f64_str(x),
            bits: Some(format!("0x{:016x}", x.to_bits())),
            ..Out::default()
        }
    });
    r.add("FloatArray.set!", |a, _, _| {
        let mut f = floats(&a[0]).to_vec();
        if let Some(i) = array::float_array_set(f.len(), sat(&a[1])) {
            f[i] = f64a(&a[2]);
        }
        list_repr(&f, |x| f64_str(*x))
    });
    r.add("ByteArray.copySlice", |a, _, _| {
        let (src, dest) = (bytes(&a[0]), bytes(&a[2]));
        let _exact = boolean(&a[5]);
        let out = match array::copy_slice(src.len(), sat(&a[1]), dest.len(), sat(&a[3]), sat(&a[4]))
        {
            None => dest.to_vec(),
            Some(p) => {
                let mut out = dest[..p.dest_start].to_vec();
                out.extend_from_slice(&src[p.src_start..p.src_start + p.len]);
                if p.dest_start + p.len < dest.len() {
                    out.extend_from_slice(&dest[p.dest_start + p.len..]);
                }
                assert_eq!(out.len(), p.new_len);
                out
            }
        };
        list_repr(&out, |x| x.to_string())
    });
}

fn panic_fns(r: &mut Registry) {
    r.add("panic", |a, _, env| {
        panic_fn(string(&a[0]), "0".into(), env)
    });
    r.add("sorryAx", |_, _, env| {
        internal_end(InternalPanic::Sorry, env)
    });
}

fn repr_fns(r: &mut Registry) {
    r.add("Nat.repr", |a, rp, _| str_repr(&nat_text(&nat(&a[0], rp))));
    r.add("USize.repr", |a, _, _| {
        let mut s = String::new();
        repr::usize_repr(nat_value(&a[0]).low_u64(), &mut s).unwrap();
        str_repr(&s)
    });
    r.add("Int.repr", |a, rp, _| str_repr(&int_text(&int(&a[0], rp))));
    r.add("Int.reprPrec", |a, rp, _| {
        let i = int(&a[0], rp);
        let prec = nat(&a[1], rp).to_u64_saturating();
        let t = int_text(&i);
        str_repr(&if repr::needs_app_paren(i.is_neg(), prec) {
            format!("({t})")
        } else {
            t
        })
    });
    for name in ["Char.quote", "Char.repr"] {
        r.add(name, |a, _, _| {
            let mut s = String::new();
            repr::char_quote(chr(&a[0]), &mut s).unwrap();
            str_repr(&s)
        });
    }
    r.add("Char.toString", |a, _, _| {
        let mut s = String::new();
        repr::char_to_string(chr(&a[0]), &mut s).unwrap();
        str_repr(&s)
    });
    r.add("String.quote", |a, _, _| {
        let mut s = String::new();
        repr::string_quote(string(&a[0]), &mut s).unwrap();
        str_repr(&s)
    });
    r.add("Bool.repr", |a, _, _| {
        str_repr(repr::bool_text(boolean(&a[0])))
    });
    r.add("toString", |a, _, _| {
        str_repr(repr::bool_text(boolean(&a[0])))
    });
    r.add("Unit.repr", |_, _, _| str_repr(repr::UNIT_TEXT));
}

// ------------------------------------------------------------------ the tests

/// Whether the call's output is the row's: `expected` (or, for a panic,
/// `panic: <first line>`, the returned `default` and the `stderr` text; for
/// an end, `ends` with its stderr and status), and a float result's bits.
fn mismatch(row: &Row, out: &Out) -> Option<String> {
    let want = match (&out.panic, &out.ends) {
        (_, Some((err, code))) => ("ends".to_string(), None, None, Some((err.clone(), *code))),
        (Some(msg), None) => (
            format!("panic: {}", msg.lines().next().unwrap_or("")),
            Some(out.value.clone()),
            Some(format!("{msg}\n")),
            None,
        ),
        (None, None) => (out.value.clone(), None, None, None),
    };
    let have = (
        row.expected.clone(),
        row.default.clone(),
        row.stderr.clone(),
        row.ends.clone(),
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

/// Every `MIRI_STRIDE`-th row of a file (the header kept), cut out of the
/// text before it is parsed: Miri interprets the reader too, and the full
/// files hold over 3000 rows.
fn miri_sample(text: &str) -> String {
    const MIRI_STRIDE: usize = 23;
    let mut parts = text.split("\n[[row]]");
    let mut out = String::from(parts.next().unwrap_or(""));
    for (k, p) in parts.enumerate() {
        if k % MIRI_STRIDE == 0 {
            out.push_str("\n[[row]]");
            out.push_str(p);
        }
    }
    out
}

/// The rows are compiled in (`include_str!`), so the tests read no files and
/// also run under Miri, on a sample (`miri_sample`) without the rows whose
/// arguments are 2^(2^32)-sized. Under Miri they run in one configuration of
/// `scripts/check.sh`, `--features unsafe-fast`, and are ignored in the
/// others (each test's `cfg_attr`): the default build has
/// `forbid(unsafe_code)` and no dependencies, so Miri cannot find undefined
/// behaviour in it, and `io` and `sched` do not change `semantics`.
fn run(file: &str, text: &str, deviations: usize) {
    let sample;
    let text = if cfg!(miri) {
        sample = miri_sample(text);
        &sample
    } else {
        text
    };
    let rows = read_rows(file, text);
    assert!(!rows.is_empty(), "{file}: no rows");
    let reg = registry();
    let mut failures = Vec::new();
    let mut seen_deviations = 0;
    for row in &rows {
        seen_deviations += row.deviation as usize;
        if cfg!(miri) && row.huge {
            continue;
        }
        let Some(f) = reg.0.get(&row.func) else {
            failures.push(format!("{}: no glue for {}", row.id, row.func));
            continue;
        };
        for rp in REPRS {
            if let Some(m) = mismatch(row, &f(&row.args, rp, &row.env)) {
                failures.push(format!(
                    "{} ({rp:?}): {} {:?}\n{m}",
                    row.id, row.func, row.args
                ));
            }
        }
    }
    if !cfg!(miri) {
        assert_eq!(
            seen_deviations, deviations,
            "{file}: the rows with deviations"
        );
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
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn nat_rows() {
    run("nat", include_str!("cases/nat/nat.rows.toml"), 11);
}

#[test]
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn int_rows() {
    run("int", include_str!("cases/int/int.rows.toml"), 0);
}

#[test]
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn array_rows() {
    run("array", include_str!("cases/array/array.rows.toml"), 5);
}

#[test]
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn panic_rows() {
    run("panic", include_str!("cases/panic/panic.rows.toml"), 0);
}

#[test]
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn repr_rows() {
    run("repr", include_str!("cases/repr/repr.rows.toml"), 0);
}

/// `refbig` against `u128`/`i128` arithmetic, on values around every limb
/// boundary, so that a row failure points at the crate, not the backend.
#[test]
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn refbig_matches_wide_arithmetic() {
    let edges: Vec<u128> = [
        0u128,
        1,
        2,
        3,
        7,
        1 << 31,
        1 << 32,
        (1 << 63) - 1,
        1 << 63,
        u64::MAX as u128,
    ]
    .iter()
    .flat_map(|&v| [v, v + 1, v.wrapping_mul(3) + 5, (v << 40) | 9])
    .filter(|v| *v < 1 << 126)
    .collect();
    // under Miri, a fifth of the values (Miri interprets every limb operation)
    let edges: Vec<u128> = edges
        .into_iter()
        .step_by(if cfg!(miri) { 5 } else { 1 })
        .collect();
    let n = |v: u128| RNat::from_u128(v);
    for &a in &edges {
        for &b in &edges {
            assert_eq!(n(a).add(n(b)).to_u128(), Some(a + b), "{a} + {b}");
            if a >= b {
                assert_eq!(n(a).sub(n(b)).to_u128(), Some(a - b), "{a} - {b}");
            }
            if let Some(p) = a.checked_mul(b) {
                assert_eq!(n(a).mul(n(b)).to_u128(), Some(p), "{a} * {b}");
            }
            if b != 0 {
                assert_eq!(n(a).div(n(b)).to_u128(), Some(a / b), "{a} / {b}");
                assert_eq!(n(a).rem(n(b)).to_u128(), Some(a % b), "{a} % {b}");
            }
            assert_eq!(n(a).and(n(b)).to_u128(), Some(a & b));
            assert_eq!(n(a).or(n(b)).to_u128(), Some(a | b));
            assert_eq!(n(a).xor(n(b)).to_u128(), Some(a ^ b));
            assert_eq!(n(a).compare(&n(b)), a.cmp(&b));
            for s in [0u64, 1, 13, 63, 64, 65, 100] {
                if s < 128 && (a << s) >> s == a {
                    assert_eq!(n(a).shl(s).to_u128(), Some(a << s), "{a} << {s}");
                }
                assert_eq!(
                    n(a).shr(s).to_u128(),
                    Some(a.checked_shr(s as u32).unwrap_or(0))
                );
            }
            let (ia, ib) = (a as i128, -(b as i128));
            let i = |v: i128| RInt::from_i128(v);
            assert_eq!(i(ia).add(i(ib)).to_i128(), Some(ia + ib));
            assert_eq!(i(ib).sub(i(ia)).to_i128(), Some(ib - ia));
            if b != 0 {
                let (q, r) = i(ia).tdiv_rem(&i(ib));
                assert_eq!((q.to_i128(), r.to_i128()), (Some(ia / ib), Some(ia % ib)));
                let (q, r) = i(-ia).tdiv_rem(&i(ib));
                assert_eq!((q.to_i128(), r.to_i128()), (Some(-ia / ib), Some(-ia % ib)));
            }
        }
        assert_eq!(
            RNat::from_decimal(&a.to_string()).to_decimal(),
            a.to_string()
        );
        assert_eq!(n(a).bit_len(), 128 - u64::from(a.leading_zeros()));
        if a != 0 {
            assert_eq!(n(a).trailing_zeros(), u64::from(a.trailing_zeros()));
        }
    }
    let big = RNat::from_decimal("340282366920938463463374607431768211457");
    assert_eq!(big.clone().gcd(RNat::from_u64(3)), RNat::from_u64(1));
    assert_eq!(
        RNat::from_u64(3).pow(80).to_decimal(),
        "147808829414345923316083210206383297601"
    );
}

/// `==` on `Nat` and `Int` is the values' equality across forms: a word and
/// a big number of the same value are equal (leanrs's review of semantics-2).
#[test]
#[cfg_attr(
    all(
        miri,
        any(not(feature = "unsafe-fast"), feature = "io", feature = "sched")
    ),
    ignore = "under Miri, rows2 runs with --features unsafe-fast only (see `run`)"
)]
fn equality_is_semantic() {
    for v in [0u128, 5, 1 << 63, u64::MAX as u128, 1 << 64] {
        let big = Nat::<RNat>::Big(RNat::from_u128(v));
        let word = u64::try_from(v).map(Nat::Small);
        if let Ok(w) = word {
            assert!(w == big && big == w, "{v}");
            assert!(w != Nat::Small(w.low_u64().wrapping_add(1)));
        }
        assert!(big == Nat::Big(RNat::from_u128(v)));
        let ibig = Int::<RInt>::Big(RInt::from_i128(-(v as i128)));
        if let Ok(x) = i64::try_from(-(v as i128)) {
            assert!(Int::Small(x) == ibig && ibig == Int::Small(x), "-{v}");
        }
        assert!(ibig != Int::Big(RInt::from_i128(v as i128 + 1)));
    }
}
