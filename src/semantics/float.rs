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
/// Source: leanrs_rt `src/float.rs` (`float_to_string`), adapted to write into
/// a caller-supplied `fmt::Write`. Rust's `{:.6}` is an exact conversion with
/// the same rounding as glibc's `%f` (rows `float/tostring.*` in
/// `tests/cases/float/float.rows.toml`).
#[inline]
pub fn to_string(x: f64, out: &mut impl fmt::Write) -> fmt::Result {
    if x.is_nan() {
        out.write_str("NaN")
    } else if x == f64::INFINITY {
        out.write_str("inf")
    } else if x == f64::NEG_INFINITY {
        out.write_str("-inf")
    } else {
        write!(out, "{x:.6}")
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
