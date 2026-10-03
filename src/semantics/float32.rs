//! `Float32` (`f32`) externs of Lean 4.34.0 that are not a single Rust
//! operator: `toString`, the saturating conversions, `ofBits`/`toBits`,
//! `frExp`, `scaleB` and the classification functions (`include/lean/lean.h`
//! and `src/runtime/object.cpp`). Signed results are returned as their
//! two's-complement encoding, as `lean.h` does.

use core::fmt;

use super::float;

/// `quietNaN32` of `src/runtime/object.cpp`: the one NaN `Float32.toBits`
/// returns.
const QUIET_NAN_BITS: u32 = 0x7FC0_0000;

/// Whether `bits` is a NaN, tested on the bits (see `float::to_bits`).
#[inline]
const fn is_nan_bits(bits: u32) -> bool {
    bits & 0x7FFF_FFFF > 0x7F80_0000
}

/// `Float32.toString` (`lean_float32_to_string`, `src/runtime/object.cpp`):
/// `std::to_string(float)`, which is `%f` of the value promoted to `double`;
/// `NaN` for every NaN, `inf`, `-inf`. Writes into `out` and allocates nothing.
///
/// Source: leanrs_rt `src/float32.rs` (`float32_to_string`), adapted to write
/// into a caller-supplied `fmt::Write`.
#[inline]
pub fn to_string(x: f32, out: &mut impl fmt::Write) -> fmt::Result {
    float::to_string(f64::from(x), out)
}

/// `Float32.toUInt8` (`lean_float32_to_uint8`, `lean.h`): NaN and values
/// below 0 give 0, values at or above 256 give 255, the rest truncate toward
/// zero; Rust's saturating `as` has exactly these semantics.
///
/// Source: new, as leanrs_rt `src/float.rs` (`to_uint8`) on `f32` (leanrs
/// writes `x as u8` inline in generated code).
#[inline]
pub fn to_uint8(x: f32) -> u8 {
    x as u8
}

/// `Float32.toUInt16` (`lean_float32_to_uint16`, `lean.h`), as `to_uint8`.
///
/// Source: new, as `to_uint8`.
#[inline]
pub fn to_uint16(x: f32) -> u16 {
    x as u16
}

/// `Float32.toUInt32` (`lean_float32_to_uint32`, `lean.h`), as `to_uint8`.
///
/// Source: new, as `to_uint8`.
#[inline]
pub fn to_uint32(x: f32) -> u32 {
    x as u32
}

/// `Float32.toUInt64` (`lean_float32_to_uint64`, `lean.h`), as `to_uint8`
/// (`2^64` and above give `UINT64_MAX`).
///
/// Source: new, as `to_uint8`.
#[inline]
pub fn to_uint64(x: f32) -> u64 {
    x as u64
}

/// `Float32.toUSize` (`lean_float32_to_usize`, `lean.h`):
/// `lean_float32_to_uint64` on 64-bit platforms.
///
/// Source: new, as `to_uint8`.
#[inline]
pub fn to_usize(x: f32) -> usize {
    x as usize
}

macro_rules! to_signed {
    ($($(#[$doc:meta])* $name:ident: $s:ty as $u:ty;)*) => {
        $(
            $(#[$doc])*
            ///
            /// NaN gives 0; otherwise truncation toward zero, saturating at
            /// the type's minimum and maximum (`lean.h`); Rust's `as` has
            /// exactly these semantics. Returned as the encoding.
            ///
            /// Source: leanrs_rt `src/float32.rs` (`to_signed!`), unchanged.
            #[inline]
            pub fn $name(x: f32) -> $u {
                (x as $s) as $u
            }
        )*
    };
}

to_signed! {
    /// `Float32.toInt8` (`lean_float32_to_int8`, `lean.h`).
    to_int8: i8 as u8;
    /// `Float32.toInt16` (`lean_float32_to_int16`, `lean.h`).
    to_int16: i16 as u16;
    /// `Float32.toInt32` (`lean_float32_to_int32`, `lean.h`).
    to_int32: i32 as u32;
    /// `Float32.toInt64` (`lean_float32_to_int64`, `lean.h`).
    to_int64: i64 as u64;
    /// `Float32.toISize` (`lean_float32_to_isize`, `lean.h`, 64-bit branch).
    to_isize: isize as usize;
}

/// `Float32.ofBits` (`lean_float32_of_bits`, `src/runtime/object.cpp`): the
/// float with these bits, every NaN replaced by the quiet NaN
/// (`std::numeric_limits<float>::quiet_NaN()`, `0x7FC00000`).
///
/// Source: new, from the C source (leanrs_rt uses `f32::from_bits`; no Lean
/// operation observes the difference, see `float::of_bits`).
#[inline]
pub fn of_bits(u: u32) -> f32 {
    if is_nan_bits(u) {
        f32::from_bits(QUIET_NAN_BITS)
    } else {
        f32::from_bits(u)
    }
}

/// `Float32.toBits` (`lean_float32_to_bits`, `src/runtime/object.cpp`): the
/// bits, with every NaN mapped to `quietNaN32` (`0x7FC00000`).
///
/// Source: leanrs_rt `src/float32.rs` (`to_bits`), unchanged.
#[inline]
pub fn to_bits(x: f32) -> u32 {
    let b = x.to_bits();
    if is_nan_bits(b) {
        QUIET_NAN_BITS
    } else {
        b
    }
}

/// `Float32.isNaN` (`lean_float32_isnan`, `src/runtime/object.cpp`).
///
/// Source: new (`f32::is_nan`).
#[inline]
pub fn isnan(x: f32) -> bool {
    x.is_nan()
}

/// `Float32.isFinite` (`lean_float32_isfinite`, `src/runtime/object.cpp`).
///
/// Source: new (`f32::is_finite`).
#[inline]
pub fn isfinite(x: f32) -> bool {
    x.is_finite()
}

/// `Float32.isInf` (`lean_float32_isinf`, `src/runtime/object.cpp`).
///
/// Source: new (`f32::is_infinite`).
#[inline]
pub fn isinf(x: f32) -> bool {
    x.is_infinite()
}

/// `Float32.frExp` (`lean_float32_frexp`, `src/runtime/object.cpp`): C's
/// `frexp` on the `float` (the `frexpf` overload), the mantissa in `[0.5, 1)`
/// with the sign of `x` and the exponent; exponent 0 for a non-finite `x`.
/// The widened value has the same mantissa and exponent, and the mantissa is
/// exact in `f32`.
///
/// Source: leanrs_rt `src/float32.rs` (`frexp`), adapted to return the
/// exponent as an `i32`.
#[inline]
pub fn frexp(x: f32) -> (f32, i32) {
    let (m, e) = float::frexp_i32(f64::from(x));
    (m as f32, e)
}

/// `Float32.scaleB` (`lean_float32_scaleb`, `src/runtime/object.cpp`):
/// `x * 2^i` with `Float.scaleB`'s argument handling (`i` saturated to `i64`
/// by the caller), computed on the widened value, where it is exact down to
/// `2^-1074`, then rounded once to `f32`, which equals `scalbnf`.
///
/// Source: leanrs_rt `src/float32.rs` (`scaleb`), adapted to take the
/// saturated `i64`.
#[inline]
pub fn scaleb(x: f32, i: i64) -> f32 {
    float::scaleb(f64::from(x), i) as f32
}
