//! `Float` (`f64`) externs of Lean 4.34.0 that are not a single Rust operator:
//! `toString`, the saturating conversions to `UIntN`/`IntN`/`USize`/`ISize`,
//! `ofBits`/`toBits`, `frExp`, `scaleB` and the classification functions.
//! The C sources are `include/lean/lean.h` (the conversions) and
//! `src/runtime/object.cpp` (the rest).
//!
//! Signed results are returned as their two's-complement encoding (`u8` for
//! `Int8`, ...), as `lean.h` returns them, since both translators hold
//! `IntN` that way. The libm functions are in `semantics::libm`.

use core::fmt;

/// `quietNaN64` of `src/runtime/object.cpp`: the one NaN `Float.toBits` returns.
const QUIET_NAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// Whether `bits` is a NaN, tested on the bits. LLVM may fold
/// `if x.is_nan() { C } else { x.to_bits() }` to `x.to_bits()` when it knows
/// `x` is a NaN of a given payload (leanrs_rt `src/float.rs`), so the test
/// must not be a float comparison.
#[inline]
const fn is_nan_bits(bits: u64) -> bool {
    bits & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000
}

/// `Float.toString` (`lean_float_to_string`, `src/runtime/object.cpp`):
/// `std::to_string(double)`, which is C's `%f`: six decimals, rounded to
/// nearest with ties to even on the exact binary value, every integer digit
/// (up to 309 of them), `-` on negative values including `-0.0` and negative
/// values that round to zero; `inf` and `-inf`; `NaN` for every NaN, whatever
/// its sign. Writes into `out` and allocates nothing.
///
/// A finite value below 2^53 in magnitude takes `fixed6`, which computes the
/// rounding exactly in integers (perf-2): for most values with few
/// significant digits (`3.0`, `0.25`), Rust's `{:.6}` gives up its fast
/// Grisu algorithm and runs the bignum Dragon one. In a cachegrind
/// micro-benchmark of the values `i / 8.0`, a call took about 1,850
/// instructions with `{:.6}` and takes about 290 with `fixed6`. The other
/// values keep `{:.6}`. The unit tests check that both give the same text,
/// on edge values and a few million pseudo-random ones.
///
/// Source: leanrs_rt `src/float.rs` (`float_to_string`), adapted to write into
/// a caller-supplied `fmt::Write`, with the fast path `fixed6` (new). Rust's
/// `{:.6}` is an exact conversion with the same rounding as glibc's `%f`
/// (rows `float/tostring.*` in `tests/cases/float/float.rows.toml`).
#[inline]
pub fn to_string(x: f64, out: &mut impl fmt::Write) -> fmt::Result {
    let bits = x.to_bits();
    if (bits >> 52) & 0x7FF < FIXED6_EXP {
        out.write_str(fixed6(bits, &mut [0; FIXED6_LEN]))
    } else if x.is_nan() {
        out.write_str("NaN")
    } else if x == f64::INFINITY {
        out.write_str("inf")
    } else if x == f64::NEG_INFINITY {
        out.write_str("-inf")
    } else {
        write!(out, "{x:.6}")
    }
}

/// The biased exponent of 2^53: a finite `f64` with a smaller one is below
/// 2^53 in magnitude, the range of `fixed6`.
const FIXED6_EXP: u64 = 1023 + 53;

/// The longest text of `fixed6`: the sign, 16 integer digits (below 2^53),
/// the point and the six decimals.
const FIXED6_LEN: usize = 24;

/// `%f` of the `f64` of `bits`, finite and below 2^53 in magnitude
/// (`to_string`'s fast path), written at the end of `buf`: `-` when the sign
/// bit is set (so also for `-0.0` and a negative value that rounds to zero),
/// the integer digits, the point and the six decimals of `round6`'s
/// rounding.
///
/// Source: new (perf-2).
fn fixed6(bits: u64, buf: &mut [u8; FIXED6_LEN]) -> &str {
    // Two digits at `buf[i..i + 2]`.
    fn pair(buf: &mut [u8; FIXED6_LEN], i: usize, d: u64) {
        let d = d as usize * 2;
        buf[i..i + 2].copy_from_slice(&super::repr::PAIRS[d..d + 2]);
    }
    let (int, frac) = round6(bits);
    let mut i = FIXED6_LEN;
    let mut f = u64::from(frac);
    for _ in 0..3 {
        i -= 2;
        pair(buf, i, f % 100);
        f /= 100;
    }
    i -= 1;
    buf[i] = b'.';
    // The integer digits, two at a time, as `repr::decimal_u64_bytes`
    // writes them, but in place.
    let mut n = int;
    while n >= 100 {
        i -= 2;
        pair(buf, i, n % 100);
        n /= 100;
    }
    if n >= 10 {
        i -= 2;
        pair(buf, i, n);
    } else {
        i -= 1;
        buf[i] = b'0' + n as u8;
    }
    if bits >> 63 == 1 {
        i -= 1;
        buf[i] = b'-';
    }
    match core::str::from_utf8(&buf[i..]) {
        Ok(s) => s,
        Err(_) => unreachable!("a sign, digits and a point are ASCII"),
    }
}

/// `n = round_half_even(|x| * 10^6)` for the `f64` of `bits`, finite and
/// below 2^53 in magnitude (the sign bit is ignored), as
/// `(n / 10^6, n % 10^6)`, computed exactly:
/// - `|x| = m * 2^-s` with integers `m < 2^53` and `0 <= s <= 1074` (a
///   subnormal has no implicit bit and `s = 1074`);
/// - the integer part is `m >> s`; the fraction is `low / 2^s`, with `low`
///   the low `s` bits of `m`;
/// - the six decimals are `low * 10^6 / 2^s` (below 10^6), rounded on the
///   exact remainder: up when it is above half of `2^s`, or exactly half
///   and the quotient odd (`n` and the decimals have the same parity, since
///   10^6 is even);
/// - `low * 10^6 < 2^73` fits a `u128`; with `s >= 128` it is below half of
///   `2^s`, so the decimals are 0 (and so is the integer part);
/// - six decimals that round up to 10^6 carry into the integer part.
///
/// Source: new (perf-2).
fn round6(bits: u64) -> (u64, u32) {
    const SCALE: u64 = 1_000_000;
    let biased = (bits >> 52) & 0x7FF;
    let field = bits & ((1 << 52) - 1);
    debug_assert!(biased < FIXED6_EXP);
    let (m, s) = if biased == 0 {
        (field, 1074)
    } else {
        (field | 1 << 52, 1075 - biased)
    };
    if s == 0 {
        // 2^52 <= |x| < 2^53: an integer.
        return (m, 0);
    }
    if s >= 128 {
        return (0, 0);
    }
    let (int, low) = if s < 64 {
        (m >> s, m & ((1 << s) - 1))
    } else {
        (0, m)
    };
    let p = u128::from(low) * u128::from(SCALE);
    let q = (p >> s) as u64;
    let rem = p & ((1 << s) - 1);
    let half = 1u128 << (s - 1);
    let q = q + u64::from(rem > half || (rem == half && q & 1 == 1));
    if q == SCALE {
        (int + 1, 0)
    } else {
        (int, q as u32)
    }
}

/// `Float.toUInt8` (`lean_float_to_uint8`, `lean.h`): NaN and values below 0
/// give 0, values at or above 256 give 255, the rest truncate toward zero.
/// Rust's saturating `as` has exactly these semantics.
///
/// Source: leanrs_rt `src/float.rs` (`to_uint8`), unchanged.
#[inline]
pub fn to_uint8(x: f64) -> u8 {
    x as u8
}

/// `Float.toUInt16` (`lean_float_to_uint16`, `lean.h`), as `to_uint8`.
///
/// Source: leanrs_rt `src/float.rs` (`to_uint16`), unchanged.
#[inline]
pub fn to_uint16(x: f64) -> u16 {
    x as u16
}

/// `Float.toUInt32` (`lean_float_to_uint32`, `lean.h`), as `to_uint8`.
///
/// Source: leanrs_rt `src/float.rs` (`to_uint32`), unchanged.
#[inline]
pub fn to_uint32(x: f64) -> u32 {
    x as u32
}

/// `Float.toUInt64` (`lean_float_to_uint64`, `lean.h`), as `to_uint8`.
///
/// Source: leanrs_rt `src/float.rs` (`to_uint64`), unchanged.
#[inline]
pub fn to_uint64(x: f64) -> u64 {
    x as u64
}

/// `Float.toUSize` (`lean_float_to_usize`, `lean.h`): `lean_float_to_uint64`
/// on 64-bit platforms.
///
/// Source: leanrs_rt `src/float.rs` (`to_usize`), unchanged.
#[inline]
pub fn to_usize(x: f64) -> usize {
    x as usize
}

macro_rules! to_signed {
    ($($(#[$doc:meta])* $name:ident: $s:ty as $u:ty;)*) => {
        $(
            $(#[$doc])*
            ///
            /// NaN gives 0; otherwise truncation toward zero, saturating at
            /// the type's minimum and maximum (`lean.h`). Rust's `as` has
            /// exactly these semantics. Returned as the two's-complement
            /// encoding, as `lean.h` does.
            ///
            /// Source: leanrs_rt `src/float.rs` (`to_signed!`), unchanged.
            #[inline]
            pub fn $name(x: f64) -> $u {
                (x as $s) as $u
            }
        )*
    };
}

to_signed! {
    /// `Float.toInt8` (`lean_float_to_int8`, `lean.h`).
    to_int8: i8 as u8;
    /// `Float.toInt16` (`lean_float_to_int16`, `lean.h`).
    to_int16: i16 as u16;
    /// `Float.toInt32` (`lean_float_to_int32`, `lean.h`).
    to_int32: i32 as u32;
    /// `Float.toInt64` (`lean_float_to_int64`, `lean.h`).
    to_int64: i64 as u64;
    /// `Float.toISize` (`lean_float_to_isize`, `lean.h`, 64-bit branch).
    to_isize: isize as usize;
}

/// `Float.ofBits` (`lean_float_of_bits`, `src/runtime/object.cpp`): the
/// double with these bits, every NaN replaced by the quiet NaN
/// (`std::numeric_limits<double>::quiet_NaN()`, `0x7FF8000000000000`).
///
/// Source: new, from the C source. leanrs_rt uses `f64::from_bits`, which keeps
/// a NaN's sign and payload; no Lean operation can observe the difference
/// (`toBits`, `toString` and every comparison treat all NaNs alike).
#[inline]
pub fn of_bits(u: u64) -> f64 {
    if is_nan_bits(u) {
        f64::from_bits(QUIET_NAN_BITS)
    } else {
        f64::from_bits(u)
    }
}

/// `Float.toBits` (`lean_float_to_bits`, `src/runtime/object.cpp`): the bits,
/// with every NaN mapped to `quietNaN64` (`0x7FF8000000000000`).
///
/// Source: leanrs_rt `src/float.rs` (`to_bits`), unchanged.
#[inline]
pub fn to_bits(x: f64) -> u64 {
    let b = x.to_bits();
    if is_nan_bits(b) {
        QUIET_NAN_BITS
    } else {
        b
    }
}

/// `Float.isNaN` (`lean_float_isnan`, `src/runtime/object.cpp`).
///
/// Source: new (`f64::is_nan`).
#[inline]
pub fn isnan(x: f64) -> bool {
    x.is_nan()
}

/// `Float.isFinite` (`lean_float_isfinite`, `src/runtime/object.cpp`).
///
/// Source: new (`f64::is_finite`).
#[inline]
pub fn isfinite(x: f64) -> bool {
    x.is_finite()
}

/// `Float.isInf` (`lean_float_isinf`, `src/runtime/object.cpp`).
///
/// Source: new (`f64::is_infinite`).
#[inline]
pub fn isinf(x: f64) -> bool {
    x.is_infinite()
}

/// C's `frexp` on the bits: the mantissa in `[0.5, 1)` with the sign of `x`,
/// and the exponent; `(x, 0)` for zeros, infinities and NaN.
///
/// Source: leanrs_rt `src/float.rs` (`frexp_i32`), unchanged.
#[inline]
pub(crate) fn frexp_i32(x: f64) -> (f64, i32) {
    let bits = x.to_bits();
    let ee = ((bits >> 52) & 0x7ff) as i32;
    if ee == 0 {
        if x == 0.0 {
            return (x, 0);
        }
        // A subnormal: scale by 2^64 into the normal range first.
        let (m, e) = frexp_i32(x * f64::from_bits(0x43f0_0000_0000_0000));
        return (m, e - 64);
    }
    if ee == 0x7ff {
        return (x, 0);
    }
    let m = (bits & 0x800f_ffff_ffff_ffff) | 0x3fe0_0000_0000_0000;
    (f64::from_bits(m), ee - 0x3fe)
}

/// `Float.frExp` (`lean_float_frexp`, `src/runtime/object.cpp`): C's
/// `frexp`, the mantissa in `[0.5, 1)` with the sign of `x` and the exponent,
/// with exponent 0 for a non-finite `x` (`isfinite(a) ? exp : 0`). Zeros give
/// `(x, 0)`, infinities and NaN `(x, 0)`.
///
/// Source: leanrs_rt `src/float.rs` (`frexp`), adapted to return the exponent
/// as an `i32` (the C `int`); the caller makes its `Int`.
#[inline]
pub fn frexp(x: f64) -> (f64, i32) {
    frexp_i32(x)
}

/// musl's `scalbn` in safe Rust: `x * 2^n`, correctly rounded, avoiding
/// double rounding in the subnormal range. `scalbn` is exact up to its one
/// final rounding, so this equals glibc's.
///
/// Source: leanrs_rt `src/float.rs` (`scalbn`), unchanged.
#[inline]
pub(crate) fn scalbn(x: f64, mut n: i32) -> f64 {
    let x1p1023 = f64::from_bits(0x7fe0_0000_0000_0000); // 2^1023
    let x1p53 = f64::from_bits(0x4340_0000_0000_0000); // 2^53
    let x1p_1022 = f64::from_bits(0x0010_0000_0000_0000); // 2^-1022
    let mut y = x;
    if n > 1023 {
        y *= x1p1023;
        n -= 1023;
        if n > 1023 {
            y *= x1p1023;
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        y *= x1p_1022 * x1p53;
        n += 1022 - 53;
        if n < -1022 {
            y *= x1p_1022 * x1p53;
            n += 1022 - 53;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    y * f64::from_bits(((0x3ff + n) as u64) << 52)
}

/// `Float.scaleB` (`lean_float_scaleb`, `src/runtime/object.cpp`): `x * 2^i`.
///
/// `i` is the `Int` argument, which the caller saturates to `i64` (only its
/// sign matters outside `i32`). Lean passes an `Int` that is a scalar (on
/// 64-bit platforms, one in the C `int` range) to `scalbn`. For a larger one
/// it returns `+0.0` when `x == 0` or `i < 0`, whatever the sign of `x` (and
/// also for a NaN `x` with `i < 0`), and `x * inf` otherwise.
///
/// Source: leanrs_rt `src/float.rs` (`scaleb`, `scaleb_big`), adapted to take
/// the saturated `i64` instead of leanrs's `Int`.
#[inline]
pub fn scaleb(x: f64, i: i64) -> f64 {
    match i32::try_from(i) {
        Ok(n) => scalbn(x, n),
        Err(_) => scaleb_big(x, i < 0),
    }
}

/// The branch of `lean_float_scaleb` for an `Int` that is a big number.
///
/// Source: leanrs_rt `src/float.rs` (`scaleb_big`), unchanged.
#[cold]
#[inline(never)]
fn scaleb_big(x: f64, negative: bool) -> f64 {
    if x == 0.0 || negative {
        0.0
    } else {
        x * f64::INFINITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// The text before perf-2's fast path: Rust's `{:.6}` for every
    /// non-NaN, non-infinite value (`fixed6` must give the same).
    fn reference(x: f64, out: &mut String) {
        out.clear();
        if x.is_nan() {
            out.push_str("NaN");
        } else if x == f64::INFINITY {
            out.push_str("inf");
        } else if x == f64::NEG_INFINITY {
            out.push_str("-inf");
        } else {
            write!(out, "{x:.6}").unwrap();
        }
    }

    /// `to_string(x)` and `reference(x)`, the same, in reused buffers.
    struct Check {
        got: String,
        want: String,
        fast: u64,
    }

    impl Check {
        fn new() -> Check {
            Check {
                got: String::new(),
                want: String::new(),
                fast: 0,
            }
        }

        fn one(&mut self, x: f64) {
            self.got.clear();
            to_string(x, &mut self.got).unwrap();
            reference(x, &mut self.want);
            assert_eq!(self.got, self.want, "bits {:#018x}", x.to_bits());
            if (x.to_bits() >> 52) & 0x7FF < FIXED6_EXP {
                self.fast += 1;
            }
        }

        /// `x`, its two neighbours (one unit in the last place away) and
        /// the three negated.
        fn around(&mut self, x: f64) {
            let b = x.abs().to_bits();
            for b in [b.saturating_sub(1), b, b + 1] {
                let y = f64::from_bits(b);
                self.one(y);
                self.one(-y);
            }
        }
    }

    /// splitmix64: the tests' fixed-seed generator.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 2^k, built from its bits (`powi` is not exact for subnormal results).
    fn pow2(k: i32) -> f64 {
        if k < -1022 {
            f64::from_bits(1 << (k + 1074))
        } else {
            f64::from_bits(((k + 1023) as u64) << 52)
        }
    }

    /// Fewer values under Miri, which interprets every instruction.
    fn n(full: u64) -> u64 {
        if cfg!(miri) {
            full / 1000 + 1
        } else {
            full
        }
    }

    /// The rows' values (`float/tostring.*`) and the special ones.
    #[test]
    fn to_string_rows() {
        let text = |x: f64| {
            let mut s = String::new();
            to_string(x, &mut s).unwrap();
            s
        };
        assert_eq!(text(0.0), "0.000000");
        assert_eq!(text(-0.0), "-0.000000");
        assert_eq!(text(-1e-9), "-0.000000");
        assert_eq!(text(0.0078125), "0.007812");
        assert_eq!(text(0.0234375), "0.023438");
        assert_eq!(text(-0.0078125), "-0.007812");
        assert_eq!(text(1.0078125), "1.007812");
        assert_eq!(text(0.9999995), "1.000000");
        assert_eq!(text(0.99999949), "0.999999");
        assert_eq!(text(999999.9999995), "999999.999999");
        assert_eq!(text(9007199254740991.0), "9007199254740991.000000");
        assert_eq!(text(9007199254740992.0), "9007199254740992.000000");
        assert_eq!(text(f64::from_bits(1)), "0.000000");
        assert_eq!(text(-f64::from_bits(1)), "-0.000000");
        assert_eq!(text(f64::INFINITY), "inf");
        assert_eq!(text(f64::NEG_INFINITY), "-inf");
        assert_eq!(text(f64::NAN), "NaN");
        assert_eq!(text(-f64::NAN), "NaN");
        assert_eq!(text(f64::from_bits(0xFFF0_0000_0000_0001)), "NaN");
    }

    /// `fixed6` against `{:.6}` on the edges of its arithmetic: zeros,
    /// subnormals, every power of two up to past 2^53, the integers near
    /// 2^52 and 2^53, the exactly representable ties (the odd multiples of
    /// 2^-7, the only `f64`s whose seventh decimal on is exactly 5), the
    /// nearest `f64`s to the ties `(k + 0.5) / 10^6` that are not
    /// representable, with integer parts up to 2^40, and the carries into
    /// the integer part; each with its neighbours and negated.
    #[test]
    fn fixed6_matches_format_on_edges() {
        let mut c = Check::new();
        for b in [0, 1, 2, 3, 0x000F_FFFF_FFFF_FFFF, 0x0010_0000_0000_0000] {
            c.around(f64::from_bits(b));
        }
        for k in -1074..=60 {
            c.around(pow2(k));
        }
        for k in 0..n(4096) {
            c.around(4503599627370496.0 + k as f64 * 0.5);
            c.around(9007199254740992.0 - k as f64);
            c.around(9007199254740992.0 + 2.0 * k as f64);
        }
        for j in (1..n(400_000)).step_by(2) {
            c.around(j as f64 / 128.0);
        }
        for j in (0..n(4096)).map(|i| (1u64 << 53) - 1 - 2 * i) {
            c.around(j as f64 / 128.0);
        }
        for int in [
            0.0,
            1.0,
            10.0,
            1000.0,
            1048576.0,
            1073741824.0,
            1099511627776.0,
        ] {
            for k in 0..n(100_000) {
                c.around(int + (k as f64 + 0.5) / 1e6);
            }
            for k in 0..n(1000) {
                c.around(int + 1.0 - (k as f64 + 0.5) / 1e6);
                c.around(int + k as f64 / 1e6);
            }
        }
        for k in 0..n(100_000) {
            c.around(k as f64);
        }
        for p in 0..=15 {
            let ten = 10f64.powi(p);
            c.around(ten);
            c.around(ten - 5e-7);
            c.around(ten - 0.5);
            c.around(ten / 1e6);
        }
        assert!(c.fast > 0);
    }

    /// `fixed6` against `{:.6}` on pseudo-random values from a fixed seed:
    /// any bits (also NaNs, infinities and values from 2^53 up, which keep
    /// `{:.6}`), random bits with a magnitude in `[2^-24, 2^53)` (where the
    /// six decimals are not all zero), random ties `j / 128` and their
    /// neighbours, and short binary fractions `k / 2^d`.
    #[test]
    fn fixed6_matches_format_on_random_values() {
        let mut c = Check::new();
        let mut s = 0x5EED_F10A_7000_0001u64;
        for _ in 0..n(2_000_000) {
            let r = next(&mut s);
            let x = match r % 4 {
                0 => f64::from_bits(next(&mut s)),
                1 => {
                    let e = 1023 - 24 + (r >> 2) % 77;
                    f64::from_bits((next(&mut s) & 0x800F_FFFF_FFFF_FFFF) | (e << 52))
                }
                2 => {
                    let j = (next(&mut s) >> 11) | 1;
                    let t = j as f64 / 128.0;
                    let b = t.to_bits();
                    f64::from_bits(match (r >> 2) % 3 {
                        0 => b - 1,
                        1 => b,
                        _ => b + 1,
                    })
                }
                _ => {
                    let d = (r >> 2) % 24;
                    let k = next(&mut s) >> (64 - 30);
                    k as f64 / (1u64 << d) as f64
                }
            };
            c.one(if r >> 63 == 1 { -x } else { x });
        }
        assert!(c.fast > n(1_000_000));
    }

    /// `Float32.toString` takes the same path on the value promoted to
    /// `f64`: random `f32` bits against `{:.6}` of the promoted value.
    #[test]
    fn float32_to_string_matches_format() {
        let mut s = 0xF32u64;
        let (mut got, mut want) = (String::new(), String::new());
        for _ in 0..n(300_000) {
            let r = next(&mut s);
            let bits = if r & 1 == 0 {
                r as u32
            } else {
                // magnitudes in [2^-24, 2^31)
                let e = 127 - 24 + ((r >> 1) as u32) % 55;
                ((r >> 32) as u32 & 0x807F_FFFF) | (e << 23)
            };
            let x = f32::from_bits(bits);
            got.clear();
            super::super::float32::to_string(x, &mut got).unwrap();
            reference(f64::from(x), &mut want);
            assert_eq!(got, want, "f32 bits {bits:#010x}");
        }
    }
}
