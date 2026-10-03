//! Runs every row of `tests/cases/<area>/<area>.rows.toml` (expected values
//! from native Lean 4.34.0, see `tests/cases/README.md`) against the crate.
//!
//! Each Lean function maps to the crate's function plus the small amount of
//! glue a translator writes around it: `Nat`/`Int` arguments reduced to the
//! `u64` the crate takes, big positions handled by `Nat` arithmetic, the
//! panic of `String.Pos.Raw.get!` reported, and the result rendered as Lean's
//! `repr`, with the bits of a float result.

#![allow(dead_code)] // some helpers serve areas whose rows come in later commits

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use lean_runtime::semantics::{float, float32, hash};

// ------------------------------------------------------------------ TOML

/// The TOML values the row files use. Integers and booleans (`ends.code`,
/// say) are read but no batch-1 row needs their value.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Vec<(String, Value)>),
}

impl Value {
    fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s,
            v => panic!("expected a string, got {v:?}"),
        }
    }
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Table(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn strings(&self) -> Vec<String> {
        match self {
            Value::Array(vs) => vs.iter().map(|v| v.as_str().to_string()).collect(),
            v => panic!("expected an array, got {v:?}"),
        }
    }
}

/// A parser for the subset of TOML the row files use: `[[row]]` headers,
/// `key = value` lines, basic and literal strings, integers, booleans,
/// one-line arrays and inline tables, and comments.
struct Toml<'a> {
    s: &'a str,
    i: usize,
}

impl Toml<'_> {
    fn err(&self, msg: &str) -> String {
        let line = self.s[..self.i].matches('\n').count() + 1;
        format!("line {line}: {msg}")
    }
    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }
    fn skip_space(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' => self.i += 1,
                '#' => self.i += self.s[self.i..].find('\n').unwrap_or(self.s.len() - self.i),
                _ => break,
            }
        }
    }
    fn skip_blank(&mut self) {
        loop {
            self.skip_space();
            match self.peek() {
                Some('\n') | Some('\r') => self.i += 1,
                _ => break,
            }
        }
    }
    fn eat(&mut self, t: &str) -> Result<(), String> {
        if self.s[self.i..].starts_with(t) {
            self.i += t.len();
            Ok(())
        } else {
            Err(self.err(&format!("expected `{t}`")))
        }
    }
    fn key(&mut self) -> Result<String, String> {
        let n = self.s[self.i..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(self.s.len() - self.i);
        if n == 0 {
            return Err(self.err("expected a key"));
        }
        self.i += n;
        Ok(self.s[self.i - n..self.i].to_string())
    }
    fn value(&mut self) -> Result<Value, String> {
        self.skip_space();
        match self.peek() {
            Some('"') => {
                self.i += 1;
                let mut out = String::new();
                loop {
                    let c = self.peek().ok_or_else(|| self.err("unterminated string"))?;
                    self.i += c.len_utf8();
                    match c {
                        '"' => return Ok(Value::Str(out)),
                        '\\' => {
                            let e = self.peek().ok_or_else(|| self.err("bad escape"))?;
                            self.i += 1;
                            let mut hex = |n: usize| -> Result<char, String> {
                                let h = &self.s[self.i..self.i + n];
                                self.i += n;
                                u32::from_str_radix(h, 16)
                                    .ok()
                                    .and_then(char::from_u32)
                                    .ok_or_else(|| format!("bad \\u escape {h}"))
                            };
                            out.push(match e {
                                '"' => '"',
                                '\\' => '\\',
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                'b' => '\u{8}',
                                'f' => '\u{c}',
                                'u' => hex(4)?,
                                'U' => hex(8)?,
                                _ => return Err(self.err("unknown escape")),
                            });
                        }
                        c => out.push(c),
                    }
                }
            }
            Some('\'') => {
                self.i += 1;
                let n = self.s[self.i..]
                    .find('\'')
                    .ok_or_else(|| self.err("unterminated literal string"))?;
                self.i += n + 1;
                Ok(Value::Str(self.s[self.i - n - 1..self.i - 1].to_string()))
            }
            Some('[') => {
                self.i += 1;
                let mut vs = Vec::new();
                loop {
                    self.skip_blank();
                    if self.peek() == Some(']') {
                        self.i += 1;
                        return Ok(Value::Array(vs));
                    }
                    vs.push(self.value()?);
                    self.skip_blank();
                    if self.peek() == Some(',') {
                        self.i += 1;
                    }
                }
            }
            Some('{') => {
                self.i += 1;
                let mut kv = Vec::new();
                loop {
                    self.skip_space();
                    if self.peek() == Some('}') {
                        self.i += 1;
                        return Ok(Value::Table(kv));
                    }
                    let k = self.key()?;
                    self.skip_space();
                    self.eat("=")?;
                    kv.push((k, self.value()?));
                    self.skip_space();
                    if self.peek() == Some(',') {
                        self.i += 1;
                    }
                }
            }
            Some('t') => self.eat("true").map(|_| Value::Bool(true)),
            Some('f') => self.eat("false").map(|_| Value::Bool(false)),
            _ => {
                let n = self.s[self.i..]
                    .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '+'))
                    .unwrap_or(self.s.len() - self.i);
                let v = self.s[self.i..self.i + n]
                    .parse()
                    .map_err(|_| self.err("bad value"))?;
                self.i += n;
                Ok(Value::Int(v))
            }
        }
    }
    /// The `[[row]]` tables of a file.
    fn rows(s: &str) -> Result<Vec<Value>, String> {
        let mut p = Toml { s, i: 0 };
        let mut rows: Vec<Value> = Vec::new();
        loop {
            p.skip_blank();
            if p.i >= s.len() {
                return Ok(rows);
            }
            if s[p.i..].starts_with("[[row]]") {
                p.i += 7;
                rows.push(Value::Table(Vec::new()));
                continue;
            }
            let k = p.key()?;
            p.skip_space();
            p.eat("=")?;
            let v = p.value()?;
            match rows.last_mut() {
                Some(Value::Table(kv)) => kv.push((k, v)),
                _ => return Err(p.err("a key outside [[row]]")),
            }
        }
    }
}

// ------------------------------------------------------------------ rows

#[derive(Clone, Debug)]
enum Arg {
    Nat(u128),
    Int(i128),
    Str(String),
    Bytes(Vec<u8>),
    F64(f64),
    F32(f32),
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
    let mut chars = text.char_indices();
    assert_eq!(chars.next().map(|c| c.1), Some('"'));
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, i + 1)),
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
            16 => Ok(Arg::F64(float::of_bits(n))),
            8 => Ok(Arg::F32(float32::of_bits(n as u32))),
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
