//! Lean 4.34.0's `String` position functions and byte-order comparisons, on
//! the string's UTF-8 bytes (without C's terminating NUL) and plain positions.
//! The C sources are `include/lean/lean.h` and `src/runtime/object.cpp`.
//!
//! A `String.Pos.Raw` is a `Nat` byte offset. Lean's C handles a position
//! below 2^63 as a scalar and a bigger one as a big number, which is always
//! past the end. Here positions are `u64`s:
//! - `utf8_next`, `utf8_next_fast` and `utf8_prev` take a position below
//!   2^63. For a bigger one C computes `p + 1` or `p - 1` with `Nat`
//!   arithmetic, which the caller does on its own `Nat` instead of calling
//!   these. (In debug builds they assert the bound; in release builds they
//!   still return `p + 1` / `p - 1` modulo 2^64 and never panic.)
//! - Every other function takes any `u64` and treats a position at or past
//!   the end as C treats a big one, so the caller passes a big position as
//!   itself if it fits in a `u64`, or as `u64::MAX`.
//!
//! The functions follow the C code on any bytes, but a Lean `String` is always
//! valid UTF-8, and the tests only use valid strings. Results are plain data:
//! a `u32` character, a `u64` position, a byte `Range`; the caller builds its
//! own string from a range.
//!
//! The `Substring.Raw` externs of 4.34.0 (`lean_substring_*`) are Lean code
//! (`@[export]` definitions), not C, so they are not here. `String.Slice`'s C
//! functions are `lean_slice_hash` (`semantics::hash::slice_hash`) and
//! `lean_slice_dec_lt` (`lt` on the two byte ranges).
//!
//! Source: lean2rr leanrt `src/string.rs` (the decoder, `next`, `prev`,
//! `is_valid_pos`: already on `&[u8]` and closest to the C code), adapted;
//! leanrs_rt `src/str.rs` for `extract`, `memcmp` and the comparisons,
//! adapted from its `Str`/`Nat` to views.

use core::cmp::Ordering;
use core::ops::Range;

/// `lean_char_default_value` (`lean.h`): `'A'`, the `Inhabited Char` value
/// that `String.Pos.Raw.get` returns at an invalid position.
pub const CHAR_DEFAULT: u32 = 'A' as u32;

/// The message `String.Pos.Raw.get!` passes to `lean_panic_fn` at an invalid
/// position (`lean_string_utf8_get_panic`, `src/runtime/object.cpp`).
pub const GET_BANG_PANIC: &str = "Error: invalid `String.Pos` at `String.get!`";

/// `is_utf8_first_byte` (`src/runtime/object.cpp`): `0xxxxxxx`, `110xxxxx`,
/// `1110xxxx` or `11110xxx`. Continuation bytes and `0xF8`..`0xFF` are not.
///
/// Source: lean2rr leanrt `src/string.rs` (`is_utf8_first_byte`), rewritten as
/// two comparisons. leanrs_rt's `is_first_byte` also accepts `0xF8`..`0xFF`,
/// which never occur in a Lean string.
#[inline]
fn is_utf8_first_byte(c: u8) -> bool {
    c < 0x80 || (0xC0..0xF8).contains(&c)
}

/// The byte size `lean_string_utf8_next` steps over at a non-ASCII byte `c`
/// (`lean_string_utf8_next_fast_cold`): 2, 3 or 4 for `110xxxxx`,
/// `1110xxxx`, `11110xxx`, otherwise 1 (a continuation byte, `0xF8`..`0xFF`).
/// Out of line, as in C: the caller's ASCII test stays a branch, so on ASCII
/// text the next position does not wait for the byte load (review RS1-02).
///
/// Source: lean2rr leanrt `src/string.rs` (`next`), rewritten on the count of
/// leading ones.
#[cold]
#[inline(never)]
fn next_step_cold(c: u8) -> u64 {
    match c.leading_ones() {
        n @ 2..=4 => u64::from(n),
        _ => 1,
    }
}

/// `lean_string_utf8_get_core` (`src/runtime/object.cpp`) for a non-ASCII
/// lead byte at `i`: decode one, two or three continuations when they are
/// within `size`, checking the value ranges (not the continuation bits, as
/// C), or `None`. Bytes at or past `s.len()` read as 0, as C reads its NUL
/// terminator (`lean_string_utf8_get_fast` passes `size = s.len() + 1`).
///
/// Source: lean2rr leanrt `src/string.rs` (`get_core`), adapted.
#[cold]
#[inline(never)]
fn get_core_cold(s: &[u8], size: usize, i: usize, c: u8) -> Option<u32> {
    let c = u32::from(c);
    let at = |k: usize| -> u32 { s.get(i + k).map_or(0, |&b| u32::from(b)) & 0x3f };
    if (c & 0xe0) == 0xc0 && i + 1 < size {
        let r = ((c & 0x1f) << 6) | at(1);
        if r >= 0x80 {
            return Some(r);
        }
    }
    if (c & 0xf0) == 0xe0 && i + 2 < size {
        let r = ((c & 0x0f) << 12) | (at(1) << 6) | at(2);
        if r >= 0x800 && !(0xD800..=0xDFFF).contains(&r) {
            return Some(r);
        }
    }
    if (c & 0xf8) == 0xf0 && i + 3 < size {
        let r = ((c & 0x07) << 18) | (at(1) << 12) | (at(2) << 6) | at(3);
        if (0x10000..=0x10FFFF).contains(&r) {
            return Some(r);
        }
    }
    None
}

/// The character at `pos`, or `None` at or past the end and where decoding
/// fails (in the middle of a character): the common part of
/// `lean_string_utf8_get`, `_get_opt` and `_get_bang`.
#[inline]
fn get_checked(s: &[u8], pos: u64) -> Option<u32> {
    let i = pos as usize;
    match s.get(i) {
        None => None,
        Some(&c) if c < 0x80 => Some(u32::from(c)),
        Some(&c) => get_core_cold(s, s.len(), i, c),
    }
}

/// `String.Pos.Raw.get` (`lean_string_utf8_get`, `src/runtime/object.cpp`):
/// the character that starts at `pos`; `'A'` (`CHAR_DEFAULT`) at or past the
/// end, at a big position and in the middle of a character.
///
/// Source: lean2rr leanrt `src/string.rs` (`get`), adapted (the ASCII test
/// inline, the decoder cold).
#[inline]
pub fn utf8_get(s: &[u8], pos: u64) -> u32 {
    match get_checked(s, pos) {
        Some(c) => c,
        None => CHAR_DEFAULT,
    }
}

/// `String.Pos.Raw.get?` (`lean_string_utf8_get_opt`,
/// `src/runtime/object.cpp`): `utf8_get` with `None` where it gives `'A'`
/// for an invalid position.
///
/// Source: leanrs_rt `src/str.rs` (`get_opt`), adapted to a view (lean2rr
/// returns `0x110000` for `none`).
#[inline]
pub fn utf8_get_opt(s: &[u8], pos: u64) -> Option<u32> {
    get_checked(s, pos)
}

/// `String.Pos.Raw.get!` (`lean_string_utf8_get_bang`,
/// `src/runtime/object.cpp`): the character, or `None` where Lean panics with
/// `GET_BANG_PANIC` (through `lean_panic_fn`, so the message is printed as
/// for any Lean panic) and then returns `'A'` (`CHAR_DEFAULT`). The caller
/// reports the panic in its own way.
///
/// Source: leanrs_rt `src/str.rs` (`get_bang`), adapted: the panic is left to
/// the caller.
#[inline]
pub fn utf8_get_bang(s: &[u8], pos: u64) -> Option<u32> {
    get_checked(s, pos)
}

/// `String.Pos.Raw.get'` and `String.decodeChar` (`lean_string_utf8_get_fast`
/// and `lean_string_utf8_get_fast_cold`, `lean.h` and
/// `src/runtime/object.cpp`): the character at `pos`, which a proof keeps
/// before the end (but `get'` allows the middle of a character, where the
/// result is `'A'`). The cold path bounds its reads by the size including C's
/// NUL terminator; at `pos == s.len()` (unreachable through the proofs) C reads
/// that NUL and returns 0, as this does.
///
/// Source: lean2rr leanrt `src/string.rs` (`get_fast`), adapted (0 at the
/// end, as C).
#[inline]
pub fn utf8_get_fast(s: &[u8], pos: u64) -> u32 {
    let i = pos as usize;
    match s.get(i) {
        None => 0,
        Some(&c) if c < 0x80 => u32::from(c),
        Some(&c) => match get_core_cold(s, s.len() + 1, i, c) {
            Some(r) => r,
            None => CHAR_DEFAULT,
        },
    }
}

/// `String.Pos.Raw.next` (`lean_string_utf8_next`, `src/runtime/object.cpp`):
/// `pos` plus the byte size of the character that starts there; `pos + 1` in
/// the middle of a character and at or past the end.
///
/// `pos` must be below 2^63 (see the module doc): for a bigger one the
/// caller computes `p + 1` on its own `Nat`, as C does. The result is at most
/// 2^63, which Lean's C returns as a big `Nat`.
///
/// Source: lean2rr leanrt `src/string.rs` (`next`), adapted: `pos + 1` past
/// the end instead of a sentinel; the ASCII test inline and the other lead
/// bytes out of line, as C's `lean_string_utf8_next_fast`.
#[inline]
pub fn utf8_next(s: &[u8], pos: u64) -> u64 {
    debug_assert!(
        pos < 1 << 63,
        "utf8_next: a position at or above 2^63 is the caller's"
    );
    match s.get(pos as usize) {
        Some(&c) if c < 0x80 => pos + 1,
        Some(&c) => pos + next_step_cold(c),
        None => pos.wrapping_add(1),
    }
}

/// `String.Pos.Raw.next'` and `String.Pos.next` (`lean_string_utf8_next_fast`
/// and `lean_string_utf8_next_fast_cold`, `lean.h` and
/// `src/runtime/object.cpp`): as `utf8_next` for a position a proof keeps
/// before the end (`pos + 1` in the middle of a character). At `pos ==
/// s.len()` C reads its NUL terminator and returns `pos + 1`, as this does.
///
/// Source: lean2rr leanrt `src/string.rs` (`next_fast`), adapted: the ASCII
/// test inline and the other lead bytes out of line, as C.
#[inline]
pub fn utf8_next_fast(s: &[u8], pos: u64) -> u64 {
    debug_assert!(pos < 1 << 63, "utf8_next_fast: a position at or above 2^63");
    match s.get(pos as usize) {
        Some(&c) if c < 0x80 => pos + 1,
        Some(&c) => pos + next_step_cold(c),
        None => pos.wrapping_add(1),
    }
}

/// `String.Pos.Raw.prev` (`lean_string_utf8_prev`, `src/runtime/object.cpp`):
/// the start of the character before `pos` (of the character containing `pos`
/// when `pos` is in the middle of one); 0 at 0; `pos - 1` past the end.
///
/// `pos` must be below 2^63 (see the module doc): for a bigger one the
/// caller computes `p - 1` on its own `Nat`, as C does.
///
/// Source: lean2rr leanrt `src/string.rs` (`prev`), adapted to search the
/// prefix with `rposition` (no bounds checks in the loop). C walks back with no
/// lower bound, relying on byte 0 being a first byte; this stops at 0.
#[inline]
pub fn utf8_prev(s: &[u8], pos: u64) -> u64 {
    debug_assert!(
        pos < 1 << 63,
        "utf8_prev: a position at or above 2^63 is the caller's"
    );
    if pos == 0 {
        0
    } else if pos > s.len() as u64 {
        pos - 1
    } else {
        // The last first byte strictly before `pos`.
        match s[..pos as usize]
            .iter()
            .rposition(|&c| is_utf8_first_byte(c))
        {
            Some(i) => i as u64,
            None => 0,
        }
    }
}

/// `String.Pos.Raw.atEnd` (`lean_string_utf8_at_end`, `lean.h`):
/// `pos >= utf8ByteSize`; true for a big position.
///
/// Source: leanrs_rt `src/str.rs` (`at_end`), adapted to a view.
#[inline]
pub fn utf8_at_end(s: &[u8], pos: u64) -> bool {
    pos >= s.len() as u64
}

/// `String.Pos.Raw.isValid` (`lean_string_is_valid_pos`,
/// `src/runtime/object.cpp`): a first byte of a character, or the end
/// position; false for a big position.
///
/// Source: lean2rr leanrt `src/string.rs` (`is_valid_pos`), adapted.
#[inline]
pub fn is_valid_pos(s: &[u8], pos: u64) -> bool {
    match s.get(pos as usize) {
        Some(&c) => is_utf8_first_byte(c),
        None => pos == s.len() as u64,
    }
}

/// `String.Pos.Raw.extract` (`lean_string_utf8_extract`,
/// `src/runtime/object.cpp`): the byte range `[b, e)` of the result. Empty
/// when `b >= e`, when `b` is at or past the end, or when `b` is in the
/// middle of a character; an `e` past the end or in the middle of a character
/// is moved to the end. A big position is past the end (the caller passes
/// `u64::MAX`, as C maps it to `SIZE_MAX`).
///
/// Source: leanrs_rt `src/str.rs` (`extract`), adapted to return the range.
#[inline]
pub fn utf8_extract(s: &[u8], b: u64, e: u64) -> Range<usize> {
    let sz = s.len();
    if b >= e || b >= sz as u64 || !is_utf8_first_byte(s[b as usize]) {
        return 0..0;
    }
    let mut e = if e > sz as u64 { sz } else { e as usize };
    if e < sz && !is_utf8_first_byte(s[e]) {
        e = sz;
    }
    b as usize..e
}

/// `String.extract` on valid positions (`lean_string_utf8_extract_fast`,
/// `src/runtime/object.cpp`, new in 4.34): the range `[b, e)`, empty when
/// `b >= e`. The positions are valid by proof; they are clamped to the string
/// here, which C asserts instead.
///
/// Source: new, from the C source.
#[inline]
pub fn utf8_extract_fast(s: &[u8], b: u64, e: u64) -> Range<usize> {
    let e = e.min(s.len() as u64);
    if b >= e {
        0..0
    } else {
        b as usize..e as usize
    }
}

/// `String.getUTF8Byte` and `String.Internal.ugetUTF8Byte`
/// (`lean_string_get_byte_fast`, `lean_string_uget_byte_fast`, `lean.h`): the
/// byte at `pos`, which a proof keeps before the end. At or past the end
/// (unreachable through the proof) C reads its NUL terminator, 0, as this
/// returns.
///
/// Source: lean2rr leanrt (the prelude's byte read, 0 out of range), adapted.
#[inline]
pub fn get_byte_fast(s: &[u8], pos: u64) -> u8 {
    match s.get(pos as usize) {
        Some(&b) => b,
        None => 0,
    }
}

/// The number of characters of UTF-8 bytes, as `lean_utf8_n_strlen`
/// (`src/runtime/utf8.cpp`) counts it when Lean makes a string (it counts by
/// lead-byte sizes; on valid UTF-8 that equals the number of bytes that are
/// not continuation bytes, which is what this counts, vectorized).
///
/// This is the count a translator caches when it makes a string, as Lean
/// caches it in `m_length`. `String.length` (`lean_string_length`) must read
/// that cached count, never call this: counting is O(n) where Lean's is O(1).
///
/// Source: leanrs_rt `src/str.rs` (`count_chars`) and lean2rr leanrt
/// `src/string.rs` (`utf8_count`), the same count, written as one filter.
#[inline]
pub fn utf8_strlen(s: &[u8]) -> u64 {
    s.iter().filter(|&&b| (b as i8) >= -0x40).count() as u64
}

/// `utf8_strlen` at compile time, for a string literal's cached count (a
/// translator's literal strings are constants).
///
/// Source: leanrs_rt `src/str.rs` (`count_chars`, a `const fn`).
#[inline]
pub const fn utf8_strlen_const(s: &[u8]) -> u64 {
    let mut i = 0;
    let mut n = 0;
    while i < s.len() {
        if (s[i] as i8) >= -0x40 {
            n += 1;
        }
        i += 1;
    }
    n
}

/// `String.Slice.Pattern.Internal.memcmpStr` (`lean_string_memcmp`,
/// `src/runtime/object.cpp`): whether the `len` bytes of `lhs` from `lstart`
/// equal those of `rhs` from `rstart`. The ranges are in bounds by proof; out
/// of bounds (unreachable) gives false here.
///
/// Source: leanrs_rt `src/str.rs` (`memcmp_str`), adapted to views.
#[inline]
pub fn memcmp(lhs: &[u8], rhs: &[u8], lstart: u64, rstart: u64, len: u64) -> bool {
    let (l, r, n) = (lstart as usize, rstart as usize, len as usize);
    match (
        lhs.get(l..l.saturating_add(n)),
        rhs.get(r..r.saturating_add(n)),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// `String.decLt` and `String.Slice`'s `<` (`lean_string_dec_lt`, which is
/// `lean_string_lt`, and `lean_slice_dec_lt`, `src/runtime/object.cpp`): the
/// byte-lexicographic order (`memcmp` of the common prefix, then the
/// lengths), which on UTF-8 is the order of code points.
///
/// Source: leanrs_rt `src/str.rs` (`dec_lt`, `slice_dec_lt`), adapted to
/// views (lean2rr leanrt `src/string.rs` `dec_lt` is the same).
#[inline]
pub fn lt(a: &[u8], b: &[u8]) -> bool {
    a < b
}

/// `compare` on strings (`lean_string_compare`, `src/runtime/object.cpp`):
/// the byte-lexicographic order. Lean's `Ordering` constructor indices are
/// `lt = 0`, `eq = 1`, `gt = 2`.
///
/// Source: leanrs_rt `src/str.rs` (`compare`), adapted to views.
#[inline]
pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `utf8_strlen_const` runs at compile time.
    const LITERAL: u64 = utf8_strlen_const("a€😀é".as_bytes());

    #[test]
    fn const_count() {
        assert_eq!(LITERAL, 4);
        assert_eq!(LITERAL, utf8_strlen("a€😀é".as_bytes()));
    }
}
