//! The text functions of `Std.Net.Addr` (Lean 4.34.0
//! `src/Std/Net/Addr.lean`): `IPv4Addr.ofString`/`toString` and
//! `IPv6Addr.ofString`/`toString`. They need no event loop. Natively they
//! are `lean_uv_pton_v4`, `lean_uv_ntop_v4`, `lean_uv_pton_v6` and
//! `lean_uv_ntop_v6` (`src/runtime/uv/net_addr.cpp`), over libuv's own
//! `uv_inet_pton` and `uv_inet_ntop` (libuv 1.48.0 `src/inet.c`, the
//! version Lean 4.34.0's toolchain links), not glibc's. These are ports of
//! that code.
//!
//! An `IPv4Addr` is its four octets and an `IPv6Addr` its eight 16-bit
//! segments, most significant first (`ofParts`): `[u8; 4]` and `[u16; 8]`
//! here. The parsers take the string's UTF-8 bytes; the formatters write
//! into a caller-supplied `fmt::Write`.
//!
//! The rules (libuv's, from the BIND code):
//! - IPv4: exactly four decimal parts of 0 to 255 separated by `.`, no
//!   leading zero (`01` is rejected, `0` is not), no sign, no space, nothing
//!   else.
//! - IPv6: up to eight groups of 1 to 4 hexadecimal digits (either case,
//!   leading zeros allowed) separated by `:`, at most one `::` for a run of
//!   zero groups (which may stand for one group), and an IPv4 address in
//!   the last 32 bits (`::ffff:1.2.3.4`, `1:2:3:4:5:6:1.2.3.4`). The text
//!   from the first `%` on (a zone, `fe80::1%eth0`) is dropped, if the text
//!   before it has at most 45 bytes. That limit cannot be observed: no
//!   address text is longer (`0000:0000:0000:0000:0000:0000:255.255.255.255`
//!   has 45 bytes), so a longer one is rejected either way.
//! - A string that contains a NUL byte is rejected by Lean before libuv
//!   sees it.
//! - `toString` of an IPv6 address writes each group in lowercase hex
//!   without leading zeros, replaces the first longest run of two or more
//!   zero groups with `::`, and writes the last 32 bits as an IPv4 address
//!   after a first run of exactly six zero groups (`::1.2.3.4`, but `::1`
//!   and `::2`) or of five followed by `ffff` (`::ffff:1.2.3.4`).
//!
//! Source: new, ported from libuv 1.48.0 `src/inet.c` (`inet_pton4`,
//! `inet_pton6`, `uv_inet_pton`, `inet_ntop4`, `inet_ntop6`) and
//! `src/runtime/uv/net_addr.cpp` (the NUL check, the octet and segment
//! order). lean2rr's leanrt `src/net.rs` (`pton`, `ntop`) calls glibc's
//! `inet_pton`/`inet_ntop` through FFI instead.

use core::fmt;

/// libuv's `UV__INET6_ADDRSTRLEN`: room for the longest IPv6 text and a NUL.
const INET6_ADDRSTRLEN: usize = 46;

/// libuv's `inet_pton4` on all of `src` (C stops at its NUL): the four
/// octets, or `None`.
fn pton4(src: &[u8]) -> Option<[u8; 4]> {
    let mut tmp = [0u8; 4];
    let mut tp = 0;
    let mut saw_digit = false;
    let mut octets = 0;
    for &ch in src {
        if ch.is_ascii_digit() {
            let nw = u32::from(tmp[tp]) * 10 + u32::from(ch - b'0');
            if saw_digit && tmp[tp] == 0 {
                return None; // a leading zero
            }
            if nw > 255 {
                return None;
            }
            tmp[tp] = nw as u8;
            if !saw_digit {
                octets += 1;
                if octets > 4 {
                    return None;
                }
                saw_digit = true;
            }
        } else if ch == b'.' && saw_digit {
            if octets == 4 {
                return None;
            }
            tp += 1;
            tmp[tp] = 0;
            saw_digit = false;
        } else {
            return None;
        }
    }
    if octets < 4 {
        return None;
    }
    Some(tmp)
}

/// The value of a hexadecimal digit, as libuv's two `strchr` tables find it.
#[inline]
fn hex_value(ch: u8) -> Option<u32> {
    match ch {
        b'0'..=b'9' => Some(u32::from(ch - b'0')),
        b'a'..=b'f' => Some(u32::from(ch - b'a') + 10),
        b'A'..=b'F' => Some(u32::from(ch - b'A') + 10),
        _ => None,
    }
}

/// libuv's `inet_pton6` on all of `src`: the sixteen bytes, or `None`.
fn pton6(src: &[u8]) -> Option<[u8; 16]> {
    const ENDP: usize = 16;
    let mut tmp = [0u8; ENDP];
    let mut tp = 0;
    let mut colonp: Option<usize> = None;
    let mut i = 0;
    // A leading `::` requires some special handling.
    if src.first() == Some(&b':') {
        i = 1;
        if src.get(1) != Some(&b':') {
            return None;
        }
    }
    let mut curtok = i;
    let mut seen_xdigits = 0;
    let mut val = 0u32;
    while let Some(&ch) = src.get(i) {
        i += 1;
        if let Some(d) = hex_value(ch) {
            val = (val << 4) | d;
            seen_xdigits += 1;
            if seen_xdigits > 4 {
                return None;
            }
            continue;
        }
        if ch == b':' {
            curtok = i;
            if seen_xdigits == 0 {
                if colonp.is_some() {
                    return None;
                }
                colonp = Some(tp);
                continue;
            } else if i == src.len() {
                return None; // a trailing single `:`
            }
            if tp + 2 > ENDP {
                return None;
            }
            tmp[tp] = (val >> 8) as u8;
            tmp[tp + 1] = val as u8;
            tp += 2;
            seen_xdigits = 0;
            val = 0;
            continue;
        }
        if ch == b'.' && tp + 4 <= ENDP {
            if let Some(v4) = pton4(&src[curtok..]) {
                tmp[tp..tp + 4].copy_from_slice(&v4);
                tp += 4;
                seen_xdigits = 0;
                break; // `inet_pton4` read to the end
            }
        }
        return None;
    }
    if seen_xdigits > 0 {
        if tp + 2 > ENDP {
            return None;
        }
        tmp[tp] = (val >> 8) as u8;
        tmp[tp + 1] = val as u8;
        tp += 2;
    }
    if let Some(colonp) = colonp {
        // Move the groups after `::` to the end, filling with zeros.
        let n = tp - colonp;
        if tp == ENDP {
            return None;
        }
        for k in 1..=n {
            tmp[ENDP - k] = tmp[colonp + n - k];
            tmp[colonp + n - k] = 0;
        }
        tp = ENDP;
    }
    if tp != ENDP {
        return None;
    }
    Some(tmp)
}

/// `Std.Net.IPv4Addr.ofString` (`lean_uv_pton_v4`,
/// `src/runtime/uv/net_addr.cpp`, over libuv's `uv_inet_pton(AF_INET, ..)`):
/// the octets of the address `s` names, or `None` (see the module doc for the
/// rules).
///
/// Source: new, ported from libuv 1.48.0 `src/inet.c` (`inet_pton4`) and
/// `lean_uv_pton_v4` (the NUL check).
#[inline]
pub fn pton_v4(s: &[u8]) -> Option<[u8; 4]> {
    if s.contains(&0) {
        return None;
    }
    pton4(s)
}

/// `Std.Net.IPv6Addr.ofString` (`lean_uv_pton_v6`,
/// `src/runtime/uv/net_addr.cpp`, over libuv's
/// `uv_inet_pton(AF_INET6, ..)`): the segments of the address `s` names, or
/// `None` (see the module doc for the rules; a zone after `%` is dropped).
///
/// Source: new, ported from libuv 1.48.0 `src/inet.c` (`uv_inet_pton`,
/// `inet_pton6`) and `lean_uv_pton_v6` (the NUL check,
/// `lean_in6_addr_to_ipv6_addr`'s segment order).
#[inline]
pub fn pton_v6(s: &[u8]) -> Option<[u16; 8]> {
    if s.contains(&0) {
        return None;
    }
    let s = match s.iter().position(|&c| c == b'%') {
        Some(p) if p > INET6_ADDRSTRLEN - 1 => return None,
        Some(p) => &s[..p],
        None => s,
    };
    let b = pton6(s)?;
    let mut segments = [0u16; 8];
    for (k, seg) in segments.iter_mut().enumerate() {
        *seg = u16::from_be_bytes([b[2 * k], b[2 * k + 1]]);
    }
    Some(segments)
}

/// A text of at most `INET6_ADDRSTRLEN` bytes on the stack.
struct Text {
    buf: [u8; INET6_ADDRSTRLEN],
    len: usize,
}

impl Text {
    fn new() -> Text {
        Text {
            buf: [0; INET6_ADDRSTRLEN],
            len: 0,
        }
    }

    fn push(&mut self, b: u8) {
        self.buf[self.len] = b;
        self.len += 1;
    }

    /// `%u` of a byte.
    fn decimal(&mut self, v: u8) {
        if v >= 100 {
            self.push(b'0' + v / 100);
        }
        if v >= 10 {
            self.push(b'0' + v / 10 % 10);
        }
        self.push(b'0' + v % 10);
    }

    /// `%x` of a group.
    fn hex(&mut self, v: u16) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut shift = 12;
        while shift > 0 && v >> shift == 0 {
            shift -= 4;
        }
        loop {
            self.push(DIGITS[usize::from(v >> shift & 15)]);
            if shift == 0 {
                break;
            }
            shift -= 4;
        }
    }

    /// `inet_ntop4`: `%u.%u.%u.%u`.
    fn ipv4(&mut self, o: [u8; 4]) {
        self.decimal(o[0]);
        for &v in &o[1..] {
            self.push(b'.');
            self.decimal(v);
        }
    }

    fn write_to<W: fmt::Write + ?Sized>(&self, out: &mut W) -> fmt::Result {
        match core::str::from_utf8(&self.buf[..self.len]) {
            Ok(s) => out.write_str(s),
            Err(_) => unreachable!("an address text is ASCII"),
        }
    }
}

/// `Std.Net.IPv4Addr.toString` (`lean_uv_ntop_v4`,
/// `src/runtime/uv/net_addr.cpp`, over libuv's `inet_ntop4`): the four octets
/// in decimal, separated by `.`.
///
/// Source: new, ported from libuv 1.48.0 `src/inet.c` (`inet_ntop4`).
#[inline]
pub fn ntop_v4<W: fmt::Write + ?Sized>(octets: [u8; 4], out: &mut W) -> fmt::Result {
    let mut t = Text::new();
    t.ipv4(octets);
    t.write_to(out)
}

/// `Std.Net.IPv6Addr.toString` (`lean_uv_ntop_v6`,
/// `src/runtime/uv/net_addr.cpp`, over libuv's `inet_ntop6`): the groups in
/// lowercase hex, the first longest run of two or more zero groups as `::`,
/// and an IPv4 tail after six zero groups or five and `ffff` (see the module
/// doc).
///
/// Source: new, ported from libuv 1.48.0 `src/inet.c` (`inet_ntop6`).
#[inline]
pub fn ntop_v6<W: fmt::Write + ?Sized>(words: [u16; 8], out: &mut W) -> fmt::Result {
    // The first longest run of zero groups, as (base, len), if it has two or more.
    let mut best: Option<(usize, usize)> = None;
    let mut cur: Option<(usize, usize)> = None;
    for (i, &w) in words.iter().enumerate() {
        if w == 0 {
            cur = Some(match cur {
                None => (i, 1),
                Some((b, l)) => (b, l + 1),
            });
        } else if let Some(c) = cur.take() {
            if best.is_none_or(|b| c.1 > b.1) {
                best = Some(c);
            }
        }
    }
    if let Some(c) = cur {
        if best.is_none_or(|b| c.1 > b.1) {
            best = Some(c);
        }
    }
    let best = best.filter(|b| b.1 >= 2);
    let mut t = Text::new();
    for (i, &w) in words.iter().enumerate() {
        if let Some((base, len)) = best {
            if i >= base && i < base + len {
                if i == base {
                    t.push(b':');
                }
                continue;
            }
        }
        if i != 0 {
            t.push(b':');
        }
        // An encapsulated IPv4 address? (A run of 7 never reaches i == 6;
        // libuv keeps the test.)
        if i == 6 {
            if let Some((0, len)) = best {
                if len == 6 || (len == 7 && words[7] != 1) || (len == 5 && words[5] == 0xffff) {
                    let [a, b] = words[6].to_be_bytes();
                    let [c, d] = words[7].to_be_bytes();
                    t.ipv4([a, b, c, d]);
                    break;
                }
            }
        }
        t.hex(w);
    }
    // A trailing run of zero groups?
    if let Some((base, len)) = best {
        if base + len == words.len() {
            t.push(b':');
        }
    }
    t.write_to(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v6(words: [u16; 8]) -> String {
        let mut s = String::new();
        ntop_v6(words, &mut s).unwrap();
        s
    }

    /// Every text `ntop_v6` writes parses back to the same address. (Safe
    /// code only, and a long loop: not under Miri.)
    #[test]
    #[cfg_attr(miri, ignore)]
    fn ntop_v6_round_trips() {
        let parts = [0u16, 1, 0xffff, 0x0102, 0xabcd];
        let mut words = [0u16; 8];
        for code in 0..5u32.pow(8) {
            let mut k = code;
            for w in words.iter_mut() {
                *w = parts[(k % 5) as usize];
                k /= 5;
            }
            let text = v6(words);
            assert!(text.len() < INET6_ADDRSTRLEN, "{text}");
            assert_eq!(pton_v6(text.as_bytes()), Some(words), "{text}");
        }
    }

    #[test]
    fn ntop_v4_extremes() {
        let mut s = String::new();
        ntop_v4([255, 255, 255, 255], &mut s).unwrap();
        assert_eq!(s, "255.255.255.255");
        assert_eq!(pton_v4(s.as_bytes()), Some([255; 4]));
    }
}
