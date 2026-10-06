//! Lean 4.34.0's `String` position functions, `String.Pos.Raw.set`,
//! `ByteArray.validateUTF8` and the byte-order comparisons, on the string's
//! UTF-8 bytes (without C's terminating NUL) and plain positions. The C
//! sources are `include/lean/lean.h`, `src/runtime/object.cpp` and
//! `src/runtime/utf8.cpp`.
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
//! a `u32` character, a `u64` position, a byte `Range`, or the change
//! `utf8_set` describes (`Utf8Set`); the caller builds its own string from a
//! range or applies the change to its own string.
//!
//! The `Substring.Raw` externs of 4.34.0 (`lean_substring_*`) are Lean code
//! (`@[export]` definitions), not C, so they are not here. `String.Slice`'s C
//! functions are `lean_slice_hash` (`semantics::hash::slice_hash`) and
//! `lean_slice_dec_lt` (`lt` on the two byte ranges).
//!
//! Source: lean2rr leanrt `src/string.rs` (the decoder, `next`, `prev`,
//! `is_valid_pos`: already on `&[u8]` and closest to the C code), adapted;
//! leanrs_rt `src/str.rs` for `extract`, `memcmp` and the comparisons,
//! adapted from its `Str`/`Nat` to views; lean2rr leanrt `src/string.rs`
//! (`set`) for `utf8_set`, restated as a change.

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
/// not continuation bytes, which is what this counts).
///
/// This is the count a translator caches when it makes a string, as Lean
/// caches it in `m_length`. `String.length` (`lean_string_length`) must read
/// that cached count, never call this: counting is O(n) where Lean's is O(1).
///
/// The continuation bytes are counted eight at a time in a `u64`
/// (`continuation_bytes`, perf-3): the whole words, then the last eight
/// bytes with those already counted masked off; below eight bytes, the
/// first four and the last four the same way, and single bytes below four.
/// The filter before (`(b as i8) >= -0x40`, counted into a `usize`) was
/// vectorized with every byte widened to a 64-bit lane: about 2 instructions
/// a byte and about 25 to start, for the few bytes of a substring.
///
/// Source: leanrs_rt `src/str.rs` (`count_chars`) and lean2rr leanrt
/// `src/string.rs` (`utf8_count`), the same count; the word loop is new
/// (perf-3).
#[inline]
pub fn utf8_strlen(s: &[u8]) -> u64 {
    let n = s.len();
    let cont = if n >= 8 {
        let (words, rest) = s.as_chunks::<8>();
        let mut c = 0u64;
        for w in words {
            c += u64::from(continuation_bytes(u64::from_le_bytes(*w)));
        }
        if !rest.is_empty() {
            // The last eight bytes; the bytes of `rest` are its top ones.
            let mut last = [0u8; 8];
            last.copy_from_slice(&s[n - 8..]);
            let keep = u64::MAX << (8 * (8 - rest.len()));
            c += u64::from(continuation_bytes(u64::from_le_bytes(last) & keep));
        }
        c
    } else if n >= 4 {
        // The first four bytes and the last four, which overlap by `8 - n`
        // bytes: the overlap is masked off the last four.
        let mut first = [0u8; 4];
        let mut last = [0u8; 4];
        first.copy_from_slice(&s[..4]);
        last.copy_from_slice(&s[n - 4..]);
        let keep = u32::MAX.checked_shl(8 * (8 - n as u32)).unwrap_or(0);
        let w =
            u64::from(u32::from_le_bytes(first)) | u64::from(u32::from_le_bytes(last) & keep) << 32;
        u64::from(continuation_bytes(w))
    } else {
        s.iter().map(|&b| u64::from(b & 0xC0 == 0x80)).sum()
    };
    n as u64 - cont
}

/// The number of continuation bytes (`10xxxxxx`) among the eight bytes of
/// `w`: bit 7 of the byte set and bit 6 clear. `w << 1` moves each byte's
/// bit 6 to its bit 7 (the bit that crosses into the next byte lands on its
/// bit 0, which is not tested).
#[inline(always)]
const fn continuation_bytes(w: u64) -> u32 {
    (w & !(w << 1) & 0x8080_8080_8080_8080).count_ones()
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

/// The change `String.Pos.Raw.set` makes (`lean_string_utf8_set`): the bytes
/// `start..end`, one character, become `new_bytes()`, the new character's
/// UTF-8 encoding. The character count stays the same (C keeps
/// `lean_string_len`).
///
/// How a glue applies it (the result is the same each way):
/// - a unique string with `same_size()`: `write_in_place`;
/// - a unique string otherwise: in place too, if its block has room for
///   `result_size` bytes: move the tail `s[end..]` to `start +
///   new_bytes().len()`, then write `new_bytes()` at `start`;
/// - a shared string: a new string from `s[..start]`, `new_bytes()` and
///   `s[end..]` (`result_size` bytes).
///
/// C updates in place only when both characters are ASCII and the string is
/// unique, and otherwise copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utf8Set {
    /// The position: the first byte of the replaced character.
    pub start: usize,
    /// The end of the replaced character: `start` plus the size its lead byte
    /// gives, at most the string's size (C's `std::string::replace` clamps
    /// the count the same way).
    pub end: usize,
    bytes: [u8; 4],
    len: u8,
    /// The new character, `None` when the code was not a scalar value
    /// (never for a Lean `Char`).
    ch: Option<char>,
}

impl Utf8Set {
    /// The new character's bytes: 1 to 4.
    ///
    /// Source: new.
    #[inline]
    pub fn new_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// `new_bytes()` as a `&str` in `buf`, for a glue whose string type
    /// takes text: the kept character's `encode_utf8`, inline, with no UTF-8
    /// check (AR-2: `core::str::from_utf8` of `new_bytes()`, out of line, ran
    /// on every `String.set`). `None` only when the character was not a
    /// scalar value, which a Lean `Char` always is.
    ///
    /// Source: new.
    #[inline]
    pub fn new_str<'b>(&self, buf: &'b mut [u8; 4]) -> Option<&'b str> {
        match self.ch {
            Some(c) => Some(c.encode_utf8(buf)),
            None => None,
        }
    }

    /// Writes the new character over the old one in `s`, the bytes of the
    /// string the change was made for, when `same_size()` (the caller's
    /// test; a debug build asserts it). One store of the character's width,
    /// with no length-dependent copy: a single byte for ASCII over ASCII, the
    /// most frequent case. Without `same_size()` the result is not the
    /// string Lean makes (wrong bytes, never undefined behaviour), and a
    /// write past the end of `s` panics.
    ///
    /// Source: new (review RS3-02: `copy_from_slice` of `new_bytes()` was a
    /// `memcpy` call even for one byte).
    #[inline]
    pub fn write_in_place(&self, s: &mut [u8]) {
        debug_assert!(self.same_size(), "Utf8Set::write_in_place: sizes differ");
        let (i, b) = (self.start, &self.bytes);
        match self.len {
            1 => s[i] = b[0],
            2 => s[i..i + 2].copy_from_slice(&b[..2]),
            3 => s[i..i + 3].copy_from_slice(&b[..3]),
            _ => s[i..i + 4].copy_from_slice(b),
        }
    }

    /// `start..end`, the replaced character's bytes.
    ///
    /// Source: new.
    #[inline]
    pub fn old_range(&self) -> Range<usize> {
        self.start..self.end
    }

    /// Whether the new character has the old one's byte size, so that a
    /// unique string can be updated in place.
    ///
    /// Source: new.
    #[inline]
    pub fn same_size(&self) -> bool {
        self.end - self.start == usize::from(self.len)
    }

    /// The byte size of the result, for the string of `size` bytes the change
    /// was made for.
    ///
    /// Source: new.
    #[inline]
    pub fn result_size(&self, size: usize) -> usize {
        size - (self.end - self.start) + usize::from(self.len)
    }
}

/// `push_unicode_scalar` (`src/runtime/utf8.cpp`): the UTF-8 bytes of `code`
/// in `d`, and their number (1 to 4). As in C, a value of 0x10000 or more
/// takes four bytes, masked (a `Char` is always a scalar value, so the masks
/// change nothing in Lean). A glue calls it where Lean's C encodes one
/// character into a string it updates itself (`String.push`,
/// `lean_string_push`); it stays inline, as it is on that hot path.
///
/// Source: lean2rr leanrt `src/string.rs` (`encode_scalar`), the same
/// encoder.
#[inline]
pub fn push_unicode_scalar(code: u32, d: &mut [u8; 4]) -> u8 {
    if code < 0x80 {
        d[0] = code as u8;
        1
    } else if code < 0x800 {
        d[0] = (code >> 6 & 0x1F) as u8 | 0xC0;
        d[1] = (code & 0x3F) as u8 | 0x80;
        2
    } else if code < 0x10000 {
        d[0] = (code >> 12 & 0x0F) as u8 | 0xE0;
        d[1] = (code >> 6 & 0x3F) as u8 | 0x80;
        d[2] = (code & 0x3F) as u8 | 0x80;
        3
    } else {
        d[0] = (code >> 18 & 0x07) as u8 | 0xF0;
        d[1] = (code >> 12 & 0x3F) as u8 | 0x80;
        d[2] = (code >> 6 & 0x3F) as u8 | 0x80;
        d[3] = (code & 0x3F) as u8 | 0x80;
        4
    }
}

/// `String.Pos.Raw.set`, `String.Pos.set` and `String.set`
/// (`lean_string_utf8_set`, `src/runtime/object.cpp`): the change that
/// replaces the character at `pos` with `c`, or `None` where Lean returns
/// the string unchanged: `pos` at or past the end, a big position (passed as
/// `u64::MAX`, see the module doc), and `pos` in the middle of a character.
/// `String.Pos.set`'s proofs keep `pos` on a character before the end.
///
/// Nothing is allocated: on `None` the caller returns its string as it is;
/// otherwise it applies the `Utf8Set`.
///
/// Source: lean2rr leanrt `src/string.rs` (`set`, `set_slow`), adapted to
/// return the change instead of updating its own string; the encoder is
/// C's `push_unicode_scalar`. ASCII over ASCII inline, the other lead bytes
/// and characters out of line (`set_cold`), as `utf8_next` (review RS3-02).
#[inline]
pub fn utf8_set(s: &[u8], pos: u64, c: u32) -> Option<Utf8Set> {
    let start = pos as usize;
    let &lead = s.get(start)?;
    if lead < 0x80 && c < 0x80 {
        // C's in-place case: one ASCII byte for another.
        return Some(Utf8Set {
            start,
            end: start + 1,
            bytes: [c as u8, 0, 0, 0],
            len: 1,
            ch: Some(c as u8 as char),
        });
    }
    set_cold(s.len(), start, lead, c)
}

/// `utf8_set` where the old or the new character is not ASCII: the old
/// character's size from its lead byte (`get_utf8_char_size_at`; `None` for
/// a continuation byte or `0xF8`..`0xFF`, which `is_utf8_first_byte`
/// rejects) and the new one's bytes.
#[cold]
#[inline(never)]
fn set_cold(size: usize, start: usize, lead: u8, c: u32) -> Option<Utf8Set> {
    let old = match lead.leading_ones() {
        0 => 1,
        n @ 2..=4 => n as usize,
        _ => return None,
    };
    let mut bytes = [0; 4];
    let len = push_unicode_scalar(c, &mut bytes);
    Some(Utf8Set {
        start,
        end: (start + old).min(size),
        bytes,
        len,
        ch: char::from_u32(c),
    })
}

/// `ByteArray.validateUTF8` (`lean_string_validate_utf8`,
/// `src/runtime/object.cpp`, over `validate_utf8` of `src/runtime/utf8.cpp`):
/// whether the bytes are UTF-8. C's validator accepts exactly the
/// well-formed UTF-8 of the Unicode standard (Table 3-7), which is what
/// `core::str::from_utf8` accepts:
/// - a lead byte `0xxxxxxx`, `110xxxxx`, `1110xxxx` or `11110xxx`, followed by
///   0, 1, 2 or 3 bytes `10xxxxxx`, all present; a continuation byte or
///   `0xF8`..`0xFF` as a lead byte is rejected;
/// - no overlong form: a 2-byte value below 0x80, a 3-byte one below 0x800
///   and a 4-byte one below 0x10000 are rejected (so `0xC0` and `0xC1` never
///   start a valid sequence);
/// - no surrogate (U+D800..U+DFFF, so no CESU-8) and nothing above U+10FFFF
///   (`0xF4 0x90`.. and the leads `0xF5`..`0xF7`).
///
/// The unit test `validate_utf8_matches_c` compares the two on every lead
/// byte and first continuation byte, with every kind of later byte.
///
/// Source: leanrs_rt `src/str.rs` (`validate_utf8`) and lean2rr leanrt
/// `src/string.rs` (`validate`), the same call. `core::str::from_utf8`
/// checks ASCII a word at a time, where C goes byte by byte.
#[inline]
pub fn validate_utf8(b: &[u8]) -> bool {
    core::str::from_utf8(b).is_ok()
}

/// Lean's lossy decoding of bytes into a string (`lean_mk_string_from_bytes`
/// and `lean_mk_string_lossy_recover`, `src/runtime/object.cpp`, which
/// `lean_decode_lossy_utf8` and `mk_string` call: the runtime decodes so
/// every text the system gives, such as arguments, paths, names and
/// environment values).
/// Valid UTF-8 is written as it is. At each byte where `validate_utf8_one`
/// fails, one U+FFFD is written, and that byte and every continuation byte
/// after it (`0x80` to `0xBF`) are skipped. Examples:
/// - `61 FF 62` gives `a\u{FFFD}b`;
/// - `C0 80` gives one U+FFFD (Rust's `String::from_utf8_lossy` gives two);
/// - `E2 28 A1` gives `\u{FFFD}(\u{FFFD}`.
///
/// The text goes to `out` in pieces (`write_str`): valid input in one piece.
/// The only error is `out`'s own. A glue that caches a character count
/// counts the result (`utf8_strlen`): each U+FFFD is one character, as in C.
///
/// Source: leanrs_rt `src/str.rs` (`from_bytes_lossy`) and lean2rr leanrt
/// `src/string.rs` (`from_bytes_lossy`), the same algorithm. `from_utf8`
/// accepts exactly what C's `validate_utf8_one` accepts (see
/// `validate_utf8`), so its `valid_up_to` is the byte where C's scan fails.
pub fn lossy_utf8<W: core::fmt::Write + ?Sized>(bytes: &[u8], out: &mut W) -> core::fmt::Result {
    let mut rest = bytes;
    loop {
        match core::str::from_utf8(rest) {
            Ok(s) => return out.write_str(s),
            Err(e) => {
                let (valid, bad) = rest.split_at(e.valid_up_to());
                if !valid.is_empty() {
                    // the prefix `from_utf8` accepted: never the default
                    out.write_str(core::str::from_utf8(valid).unwrap_or_default())?;
                }
                out.write_str("\u{FFFD}")?;
                let skip = 1 + bad[1..].iter().take_while(|&&c| c & 0xC0 == 0x80).count();
                rest = &bad[skip..];
            }
        }
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

    /// `utf8_strlen` counts as the filter of bytes that are not continuation
    /// bytes, for every length up to 40 (every path: single bytes, the two
    /// overlapping halves, whole words and the masked last word), with one
    /// continuation byte at each position and with pseudo-random bytes from
    /// every class (ASCII, continuation, lead bytes, and 0xF8 to 0xFF, which
    /// are not UTF-8: the count is defined on any bytes).
    #[test]
    fn utf8_strlen_counts_every_length_and_position() {
        fn filter(s: &[u8]) -> u64 {
            s.iter().filter(|&&b| (b as i8) >= -0x40).count() as u64
        }
        let classes = [
            0x00u8, 0x41, 0x7F, 0x80, 0x9F, 0xBF, 0xC0, 0xC3, 0xDF, 0xE2, 0xEF, 0xF0, 0xF4, 0xF8,
            0xFF,
        ];
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut next = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let mut buf = [0u8; 48];
        for n in 0..=40usize {
            for i in 0..n {
                for (a, c) in [(b'a', 0x80u8), (0x80, b'a'), (0xE2, 0xBF)] {
                    let mut v = vec![a; n];
                    v[i] = c;
                    assert_eq!(utf8_strlen(&v), filter(&v), "n {n} at {i}: {v:x?}");
                }
            }
            for _ in 0..200 {
                for b in buf.iter_mut() {
                    *b = classes[(next() % classes.len() as u64) as usize];
                }
                let off = (next() % 8) as usize;
                let v = &buf[off..off + n];
                assert_eq!(utf8_strlen(v), filter(v), "{v:x?}");
            }
        }
    }

    /// `utf8_strlen_const` runs at compile time.
    const LITERAL: u64 = utf8_strlen_const("a€😀é".as_bytes());

    #[test]
    fn const_count() {
        assert_eq!(LITERAL, 4);
        assert_eq!(LITERAL, utf8_strlen("a€😀é".as_bytes()));
    }

    /// `validate_utf8_one` of `src/runtime/utf8.cpp` (4.34.0), line by line.
    fn c_validate_one(s: &[u8], pos: &mut usize) -> bool {
        let size = s.len();
        let c = u32::from(s[*pos]);
        let at = |k: usize| u32::from(s[*pos + k]);
        if c & 0x80 == 0 {
            *pos += 1;
        } else if c & 0xe0 == 0xc0 {
            if *pos + 1 >= size {
                return false;
            }
            let c1 = at(1);
            if c1 & 0xc0 != 0x80 {
                return false;
            }
            if ((c & 0x1f) << 6) | (c1 & 0x3f) < 0x80 {
                return false;
            }
            *pos += 2;
        } else if c & 0xf0 == 0xe0 {
            if *pos + 2 >= size {
                return false;
            }
            let (c1, c2) = (at(1), at(2));
            if c1 & 0xc0 != 0x80 || c2 & 0xc0 != 0x80 {
                return false;
            }
            let r = ((c & 0x0f) << 12) | ((c1 & 0x3f) << 6) | (c2 & 0x3f);
            if r < 0x800 || (0xD800..=0xDFFF).contains(&r) {
                return false;
            }
            *pos += 3;
        } else if c & 0xf8 == 0xf0 {
            if *pos + 3 >= size {
                return false;
            }
            let (c1, c2, c3) = (at(1), at(2), at(3));
            if c1 & 0xc0 != 0x80 || c2 & 0xc0 != 0x80 || c3 & 0xc0 != 0x80 {
                return false;
            }
            let r = ((c & 0x07) << 18) | ((c1 & 0x3f) << 12) | ((c2 & 0x3f) << 6) | (c3 & 0x3f);
            if !(0x10000..=0x10FFFF).contains(&r) {
                return false;
            }
            *pos += 4;
        } else {
            return false;
        }
        true
    }

    fn c_validate(s: &[u8]) -> bool {
        let mut pos = 0;
        while pos < s.len() {
            if !c_validate_one(s, &mut pos) {
                return false;
            }
        }
        true
    }

    /// `validate_utf8` against C's validator: every string of one and two
    /// bytes, and every lead byte and first continuation byte followed by
    /// one or two bytes of each kind. A third or fourth byte matters only
    /// through whether it is a continuation byte (the value bounds C checks,
    /// 0x800, U+D800..U+DFFF, 0x10000 and 0x110000, are multiples of 0x40,
    /// so the first two bytes decide them), or as the start of the next
    /// character.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn validate_utf8_matches_c() {
        let kinds = [
            0x00, 0x41, 0x7F, 0x80, 0xBF, 0xC0, 0xC2, 0xE0, 0xED, 0xF0, 0xF4, 0xFF,
        ];
        for a in 0..=255u8 {
            assert_eq!(validate_utf8(&[a]), c_validate(&[a]), "{a:02x}");
            for b in 0..=255u8 {
                assert_eq!(
                    validate_utf8(&[a, b]),
                    c_validate(&[a, b]),
                    "{a:02x} {b:02x}"
                );
                for &c in &kinds {
                    let s = [a, b, c];
                    assert_eq!(validate_utf8(&s), c_validate(&s), "{s:02x?}");
                    for &d in &kinds {
                        let s = [a, b, c, d];
                        assert_eq!(validate_utf8(&s), c_validate(&s), "{s:02x?}");
                    }
                }
            }
        }
        // past the word-at-a-time ASCII prefix of `from_utf8`
        let mut long = [b'a'; 40];
        for i in 0..long.len() {
            for bad in [0x80, 0xC0, 0xF8] {
                long[i] = bad;
                assert_eq!(validate_utf8(&long), c_validate(&long), "{i} {bad:02x}");
                long[i] = b'a';
            }
        }
    }

    /// `utf8_set` applied as a glue applies it, against the Lean definition
    /// (`Pos.Raw.utf8SetAux`: the character that starts at the position is
    /// replaced; the string is unchanged at any other position).
    #[test]
    fn set_matches_definition() {
        let s = "a€😀é\u{7ff}\u{800}\u{ffff}\u{10000}";
        let chars = [
            'b',
            '\u{80}',
            '\u{7ff}',
            '\u{800}',
            '\u{ffff}',
            '\u{10000}',
            '\u{10ffff}',
        ];
        for pos in 0..=s.len() as u64 + 1 {
            for &c in &chars {
                let want: String = if s.is_char_boundary(pos as usize) && (pos as usize) < s.len() {
                    let mut t = String::from(&s[..pos as usize]);
                    t.push(c);
                    let rest = &s[pos as usize..];
                    t.push_str(&rest[rest.chars().next().map_or(0, char::len_utf8)..]);
                    t
                } else {
                    s.to_string()
                };
                let got = match utf8_set(s.as_bytes(), pos, c as u32) {
                    None => s.as_bytes().to_vec(),
                    Some(p) => {
                        let mut v = Vec::with_capacity(p.result_size(s.len()));
                        v.extend_from_slice(&s.as_bytes()[..p.start]);
                        v.extend_from_slice(p.new_bytes());
                        v.extend_from_slice(&s.as_bytes()[p.end..]);
                        assert_eq!(v.len(), p.result_size(s.len()));
                        assert_eq!(p.same_size(), p.old_range().len() == c.len_utf8());
                        assert_eq!(
                            p.new_str(&mut [0; 4]),
                            Some(c.encode_utf8(&mut [0; 4]) as &str)
                        );
                        assert_eq!(
                            p.new_str(&mut [0; 4]).map(str::as_bytes),
                            Some(p.new_bytes())
                        );
                        if p.same_size() {
                            let mut w = s.as_bytes().to_vec();
                            p.write_in_place(&mut w);
                            assert_eq!(w, v);
                        }
                        v
                    }
                };
                assert_eq!(got, want.as_bytes(), "{pos} {c:?}");
                assert_eq!(utf8_strlen(&got), utf8_strlen(s.as_bytes()));
            }
        }
        assert_eq!(utf8_set(s.as_bytes(), u64::MAX, 'x' as u32), None);
        // A lead byte whose character is cut off by the end: the change ends
        // at the end, as C's `std::string::replace` clamps its count.
        let cut = b"a\xe2\x82";
        let p = utf8_set(cut, 1, 'x' as u32).unwrap();
        assert_eq!(p.old_range(), 1..3);
        let mut v = cut[..p.start].to_vec();
        v.extend_from_slice(p.new_bytes());
        v.extend_from_slice(&cut[p.end..]);
        assert_eq!(v, b"ax");
        // A code that is not a scalar value (no Lean `Char` is): C's masked
        // bytes, and no `&str`.
        let p = utf8_set(b"a", 0, 0xD800).unwrap();
        assert_eq!(p.new_bytes(), b"\xed\xa0\x80");
        assert_eq!(p.new_str(&mut [0; 4]), None);
    }

    /// `lean_mk_string_from_bytes` (4.34.0), line by line: `validate_utf8`,
    /// then `lean_mk_string_lossy_recover` from the first failure.
    fn c_lossy(s: &[u8]) -> Vec<u8> {
        let mut pos = 0;
        while pos < s.len() && c_validate_one(s, &mut pos) {}
        if pos == s.len() {
            return s.to_vec();
        }
        let mut out = s[..pos].to_vec();
        let mut start = pos;
        while pos < s.len() {
            if !c_validate_one(s, &mut pos) {
                out.extend_from_slice(&s[start..pos]);
                out.extend_from_slice("\u{fffd}".as_bytes());
                pos += 1;
                while pos < s.len() && s[pos] & 0xc0 == 0x80 {
                    pos += 1;
                }
                start = pos;
            }
        }
        out.extend_from_slice(&s[start..pos]);
        out
    }

    fn lossy(b: &[u8]) -> String {
        let mut out = String::new();
        lossy_utf8(b, &mut out).unwrap();
        out
    }

    /// `lossy_utf8` against C's decoder: every string of one and two bytes,
    /// and every pair followed by one or two bytes of each kind (as in
    /// `validate_utf8_matches_c`), then the same after valid prefixes and
    /// between valid characters.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn lossy_utf8_matches_c() {
        let kinds = [
            0x00, 0x41, 0x7F, 0x80, 0xBF, 0xC0, 0xC2, 0xE0, 0xED, 0xF0, 0xF4, 0xFF,
        ];
        let check = |s: &[u8]| assert_eq!(lossy(s).as_bytes(), c_lossy(s), "{s:02x?}");
        check(&[]);
        for a in 0..=255u8 {
            check(&[a]);
            for b in 0..=255u8 {
                check(&[a, b]);
                for &c in &kinds {
                    check(&[a, b, c]);
                    for &d in &kinds {
                        check(&[a, b, c, d]);
                    }
                }
            }
        }
        for &a in &kinds {
            for &b in &kinds {
                let mut s = "x€".as_bytes().to_vec();
                s.extend_from_slice(&[a, b]);
                s.extend_from_slice("😀y".as_bytes());
                s.extend_from_slice(&[b, a]);
                check(&s);
            }
        }
    }

    /// The bytes leanrs's io probe decoded natively (Lean 4.34.0's
    /// `lean_mk_string_from_bytes`), each with the code points it gave, and
    /// lean2rr's unit case. A U+FFFD is one character.
    #[test]
    fn lossy_utf8_native_cases() {
        let cases: &[(&[u8], &[u32])] = &[
            (&[65], &[0x41]),
            (&[192, 128], &[0xfffd]),
            (&[192, 128, 128, 65], &[0xfffd, 0x41]),
            (&[226, 130], &[0xfffd]),
            (&[226, 130, 65], &[0xfffd, 0x41]),
            (&[226, 130, 172], &[0x20ac]),
            (&[237, 160, 128], &[0xfffd]),
            (&[240, 159, 152, 128], &[0x1f600]),
            (&[240, 159, 152], &[0xfffd]),
            (&[244, 144, 128, 128], &[0xfffd]),
            (&[248, 128, 128, 128, 128], &[0xfffd]),
            (&[255], &[0xfffd]),
            (&[128], &[0xfffd]),
            (&[128, 128, 65], &[0xfffd, 0x41]),
            (&[65, 255, 66], &[0x41, 0xfffd, 0x42]),
            (&[194], &[0xfffd]),
            (&[194, 65], &[0xfffd, 0x41]),
            (&[224, 128, 128], &[0xfffd]),
            (&[224, 160, 128], &[0x800]),
            (&[240, 128, 128, 128], &[0xfffd]),
            (&[195, 169, 255, 195, 169], &[0xe9, 0xfffd, 0xe9]),
            (&[254, 255], &[0xfffd, 0xfffd]),
            (&[226, 40, 161], &[0xfffd, 0x28, 0xfffd]),
            (&[240, 40, 140, 188], &[0xfffd, 0x28, 0xfffd]),
            (&[241, 128, 128], &[0xfffd]),
            (&[0], &[0]),
            (&[65, 0, 66], &[0x41, 0, 0x42]),
            (&[], &[]),
            // lean2rr's case
            (b"a\xffb\xe2\x82", &[0x61, 0xfffd, 0x62, 0xfffd]),
        ];
        for &(bytes, points) in cases {
            let got = lossy(bytes);
            let got_points: Vec<u32> = got.chars().map(u32::from).collect();
            assert_eq!(got_points, points, "{bytes:?}");
            assert_eq!(utf8_strlen(got.as_bytes()), points.len() as u64);
        }
    }

    /// The writer's error ends the decoding and is returned.
    #[test]
    fn lossy_utf8_passes_the_writers_error() {
        struct Full(usize);
        impl core::fmt::Write for Full {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                if self.0 == 0 {
                    return Err(core::fmt::Error);
                }
                self.0 -= 1;
                let _ = s;
                Ok(())
            }
        }
        assert!(lossy_utf8(b"a\xffb", &mut Full(1)).is_err());
        assert!(lossy_utf8(b"a\xffb", &mut Full(3)).is_ok());
        assert!(lossy_utf8(b"ab", &mut Full(0)).is_err());
    }

    /// `push_unicode_scalar` is `char::encode_utf8` on scalar values.
    #[test]
    fn push_unicode_scalar_encodes_scalars() {
        for c in [
            '\0',
            'a',
            '\u{7f}',
            '\u{80}',
            '\u{7ff}',
            '\u{800}',
            '\u{ffff}',
            '\u{10000}',
            '\u{10ffff}',
        ] {
            let mut d = [0; 4];
            let n = push_unicode_scalar(c as u32, &mut d);
            assert_eq!(
                &d[..usize::from(n)],
                c.encode_utf8(&mut [0; 4]).as_bytes(),
                "{c:?}"
            );
        }
    }
}
