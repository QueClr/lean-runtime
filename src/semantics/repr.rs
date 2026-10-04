//! The text of leaf values: `USize.repr` (`lean_string_of_usize`, the one C
//! formatter for integers), and the Lean definitions both translators replace
//! with runtime code, so that the replacement is written once: decimal digits
//! for `Nat.repr`/`Int.repr` (`semantics::nat::write_decimal`,
//! `semantics::int::write_decimal`), `Char.quote`, `String.quote`,
//! `Repr.addAppParen`'s test and the `Bool` and `Unit` texts
//! (`Init/Data/Repr.lean`, `Init/Data/Int/Repr.lean`, 4.34.0).
//!
//! `Float.toString` is `semantics::float`'s (batch 1). Everything else in
//! `Repr` and `ToString` (structures, lists, `Option`, `Format`'s layout) is
//! Lean code that the translators compile.
//!
//! Source: leanrs_rt `src/fmt.rs` (`char_repr`, `add_app_paren`, the leaf
//! table) and `src/nat.rs` (`decimal_u64`); the escapes follow
//! `Char.quoteCore`.

use core::fmt;

/// Lean's `max_prec` (1024): `Repr.addAppParen` parenthesizes at this
/// precedence or above.
pub const MAX_PREC: u32 = 1024;

/// The two-digit groups 00..99, for `decimal_u64`.
const PAIRS: &[u8; 200] = b"0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// The decimal digits of `n`, written at the end of `buf`, as a `&str`
/// (`std::to_string` in `lean_string_of_usize`). Two digits per step, as
/// libc++'s `to_chars` and Rust's own formatter do.
///
/// Source: leanrs_rt `src/nat.rs` (`decimal_u64`), returning the digits in
/// the caller's buffer instead of a `String`, and rewritten with two digits
/// per step (one per step was half the speed of C's).
#[inline]
pub fn decimal_u64(mut n: u64, buf: &mut [u8; 20]) -> &str {
    let mut i = buf.len();
    while n >= 100 {
        let d = (n % 100) as usize * 2;
        n /= 100;
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[d..d + 2]);
    }
    if n >= 10 {
        let d = n as usize * 2;
        i -= 2;
        buf[i..i + 2].copy_from_slice(&PAIRS[d..d + 2]);
    } else {
        i -= 1;
        buf[i] = b'0' + n as u8;
    }
    // The bytes are ASCII digits.
    match core::str::from_utf8(&buf[i..]) {
        Ok(s) => s,
        Err(_) => unreachable!("decimal digits are ASCII"),
    }
}

/// `USize.repr` (`lean_string_of_usize`): the decimal digits. `Nat.repr`
/// uses it for a value below 2^64 (`Nat.reprFast`).
///
/// Source: leanrs_rt `src/fmt.rs` (`usize_repr`), writing into the caller's
/// buffer.
#[inline]
pub fn usize_repr<W: fmt::Write + ?Sized>(n: u64, out: &mut W) -> fmt::Result {
    out.write_str(decimal_u64(n, &mut [0; 20]))
}

/// `Repr.addAppParen`'s test for a value that needs parentheses only when
/// negative (`instReprInt`, `instReprFloat`, the `IntN` instances): `true`
/// when the text goes in parentheses.
///
/// Source: leanrs_rt `src/fmt.rs` (`add_app_paren`), as a test.
#[inline]
pub fn needs_app_paren(negative: bool, prec: u32) -> bool {
    negative && prec >= MAX_PREC
}

/// `Bool.repr` and `toString` of a `Bool`.
#[inline]
pub fn bool_text(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// `repr ()` and `toString ()`.
pub const UNIT_TEXT: &str = "()";

/// `Char.quoteCore c inString`: `\n`, `\t`, `\\` and `\"` escaped, `\'`
/// outside a string, `\xNN` (two lowercase hex digits) for U+0000..U+001F and
/// U+007F, the character itself otherwise. No other character is escaped,
/// U+0080..U+009F and U+FFFF included.
///
/// Source: leanrs_rt `src/str.rs` (`quote_char_into`), checked against
/// `Init/Data/Repr.lean`.
#[inline]
pub fn char_quote_core<W: fmt::Write + ?Sized>(
    c: char,
    in_string: bool,
    out: &mut W,
) -> fmt::Result {
    match c {
        '\n' => out.write_str("\\n"),
        '\t' => out.write_str("\\t"),
        '\\' => out.write_str("\\\\"),
        '"' => out.write_str("\\\""),
        '\'' if !in_string => out.write_str("\\'"),
        '\0'..='\x1f' | '\x7f' => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let n = c as usize;
            let esc = [b'\\', b'x', HEX[n / 16], HEX[n % 16]];
            match core::str::from_utf8(&esc) {
                Ok(s) => out.write_str(s),
                Err(_) => unreachable!("an ASCII escape"),
            }
        }
        c => out.write_char(c),
    }
}

/// `Char.quote` and `repr` of a `Char`: `'`, `Char.quoteCore c`, `'`.
///
/// Source: leanrs_rt `src/fmt.rs` (`char_repr`), writing into the caller's
/// buffer.
pub fn char_quote<W: fmt::Write + ?Sized>(c: char, out: &mut W) -> fmt::Result {
    out.write_char('\'')?;
    char_quote_core(c, false, out)?;
    out.write_char('\'')
}

/// `toString` of a `Char` (`Char.toString`, `String.singleton`).
///
/// Source: leanrs_rt `src/fmt.rs` (`char_to_string`), writing into the
/// caller's buffer.
#[inline]
pub fn char_to_string<W: fmt::Write + ?Sized>(c: char, out: &mut W) -> fmt::Result {
    out.write_char(c)
}

/// `String.quote` and `repr` of a `String`: `"`, each character's
/// `Char.quoteCore c (inString := true)`, `"`.
///
/// Source: leanrs_rt `src/str.rs` (`quote`), writing into the caller's
/// buffer.
pub fn string_quote<W: fmt::Write + ?Sized>(s: &str, out: &mut W) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        char_quote_core(c, true, out)?;
    }
    out.write_char('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(c: char) -> String {
        let mut s = String::new();
        char_quote(c, &mut s).unwrap();
        s
    }

    /// Values of `tests/cases/repr`.
    #[test]
    fn leaf_text() {
        assert_eq!(decimal_u64(0, &mut [0; 20]), "0");
        assert_eq!(decimal_u64(u64::MAX, &mut [0; 20]), "18446744073709551615");
        for n in [
            1,
            9,
            10,
            99,
            100,
            101,
            999,
            1000,
            10u64.pow(19),
            10u64.pow(19) - 1,
        ] {
            assert_eq!(decimal_u64(n, &mut [0; 20]), n.to_string());
        }
        assert_eq!(quote('a'), "'a'");
        assert_eq!(quote('\''), "'\\''");
        assert_eq!(quote('\0'), "'\\x00'");
        assert_eq!(quote('\x7f'), "'\\x7f'");
        assert_eq!(quote('\r'), "'\\x0d'");
        assert_eq!(quote('\u{80}'), "'\u{80}'");
        let mut s = String::new();
        string_quote("it's \"x\"\n", &mut s).unwrap();
        assert_eq!(s, "\"it's \\\"x\\\"\\n\"");
        assert!(needs_app_paren(true, MAX_PREC) && !needs_app_paren(true, MAX_PREC - 1));
        assert!(!needs_app_paren(false, MAX_PREC));
    }
}
