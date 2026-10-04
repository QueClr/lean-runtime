//! What the row runners (`tests/rows.rs`, `tests/rows2.rs`) share: a reader
//! for the row files (`tests/cases/<area>/<area>.rows.toml`, format in
//! `tests/cases/README.md`).
//!
//! Moved out of `tests/rows.rs` unchanged apart from visibility and two
//! accessors (`as_int`, `entries`) for the `ends` and `env` fields.

#![allow(dead_code)] // each runner uses a different part

// ------------------------------------------------------------------ TOML

/// The TOML values the row files use.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Vec<(String, Value)>),
}

impl Value {
    pub fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s,
            v => panic!("expected a string, got {v:?}"),
        }
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Table(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn strings(&self) -> Vec<String> {
        match self {
            Value::Array(vs) => vs.iter().map(|v| v.as_str().to_string()).collect(),
            v => panic!("expected an array, got {v:?}"),
        }
    }
}

/// A parser for the subset of TOML the row files use: `[[row]]` headers,
/// `key = value` lines, basic and literal strings, integers, booleans,
/// one-line arrays and inline tables, and comments.
pub struct Toml<'a> {
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
    pub fn rows(s: &str) -> Result<Vec<Value>, String> {
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

impl Value {
    /// The integer of an `Int` value.
    pub fn as_int(&self) -> i64 {
        match self {
            Value::Int(i) => *i,
            v => panic!("expected an integer, got {v:?}"),
        }
    }

    /// The key-value pairs of an inline table.
    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Table(kv) => kv,
            v => panic!("expected a table, got {v:?}"),
        }
    }
}
