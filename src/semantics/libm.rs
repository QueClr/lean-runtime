//! The libm externs of `Float` and `Float32` (Lean 4.34.0
//! `src/Init/Data/Float.lean`, `src/Init/Data/Float32.lean`: `@[extern "sin"]`
//! and so on), one function per C function, named as it is.
//!
//! Native Lean calls the platform's libm (glibc). So do Rust's `f64`/`f32`
//! methods, with these exceptions, which this module handles:
//!
//! - `cbrt`, `cbrtf`: Rust's are `core::f64::math::cbrt`, correctly rounded,
//!   not glibc's; here they are ports of glibc's algorithm.
//! - `atanh`, `atanhf`: Rust's are a different formula; here they are ports of
//!   glibc's over `log1p`/`log1pf`.
//! - Constant operands. LLVM evaluates a libm call with constant operands at
//!   compile time: `f32` calls by a `double` evaluation rounded to `float`,
//!   which can differ from glibc's `*f` function in the last bit, and `pow`
//!   with a constant exponent is rewritten (`sqrt`, `x * x`, `1 / x`). Native
//!   Lean always passes run-time values to the C call. So `pow`, `powf` and
//!   every inexact `*f` function here pass their operands through
//!   `core::hint::black_box`, and the call stays a call after inlining into a
//!   translator's code. The exact ones (`fabsf`, `ceilf`, `floorf`, `roundf`,
//!   `sqrtf`) fold to glibc's result and need no barrier.
//!
//! Every other `f64` function is the `f64` method, which calls glibc.
//!
//! Source: leanrs_rt `src/libm.rs` (the `f32` functions), `src/float.rs`
//! (`cbrt`, `atanh`, `pow`) and `src/float32.rs` (`cbrt`, `atanh`); the plain
//! `f64` functions are new, as the methods leanrs's generated code calls.
//! lean2rr's leanrt calls glibc's `cbrt`, `acosh`, `asinh`, `atanh` (and
//! `f32` twins) through FFI (`src/float.rs`), which this crate cannot do
//! without `unsafe`.

use core::hint::black_box;

use super::float::{frexp_i32, scalbn};

macro_rules! f64_unary {
    ($($name:ident => $method:ident),* $(,)?) => {
        $(
            #[doc = concat!("`Float` extern `", stringify!($name), "`: glibc's `",
                stringify!($name), "`, through `f64::", stringify!($method), "`.")]
            ///
            /// Source: new (the `f64` method leanrs's generated code calls).
            #[inline]
            pub fn $name(x: f64) -> f64 {
                x.$method()
            }
        )*
    };
}

f64_unary! {
    fabs => abs, acos => acos, acosh => acosh, asin => asin, asinh => asinh, atan => atan,
    ceil => ceil, cos => cos, cosh => cosh, exp => exp, exp2 => exp2, floor => floor,
    log => ln, log10 => log10, log2 => log2, round => round, sin => sin, sinh => sinh,
    sqrt => sqrt, tan => tan, tanh => tanh,
}

/// `Float.atan2` (extern `atan2`): glibc's `atan2(y, x)`.
///
/// Source: new (`f64::atan2`).
#[inline]
pub fn atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

/// `Float.pow` (extern `pow`): glibc's `pow` on every call. LLVM rewrites a
/// `pow` with a constant operand (`x ^ 0.5` to `sqrt`, `x ^ 2.0` to `x * x`,
/// `x ^ -1.0` to `1.0 / x`, `2.0 ^ y` to `exp2`), whose results can differ
/// from glibc's in the last bit; `black_box` hides both operands.
///
/// Source: leanrs_rt `src/float.rs` (`pow`), unchanged.
#[inline]
pub fn pow(x: f64, y: f64) -> f64 {
    black_box(x).powf(black_box(y))
}

/// `2^(1/3)` and `2^(2/3)`, glibc's `CBRT2` and `SQR_CBRT2`.
const CBRT2: f64 = 1.259_921_049_894_873_2;
const SQR_CBRT2: f64 = 1.587_401_051_968_199_6;
const CBRT_FACTOR: [f64; 5] = [1.0 / SQR_CBRT2, 1.0 / CBRT2, 1.0, CBRT2, SQR_CBRT2];

/// `Float.cbrt` (extern `cbrt`): glibc's `sysdeps/ieee754/dbl-64/s_cbrt.c` as
/// its aarch64 build computes it, the polynomial and the two sums fused
/// (`mul_add`, one rounding each). Rust's `f64::cbrt` is correctly rounded
/// and differs from glibc's on about half of all inputs.
///
/// Source: leanrs_rt `src/float.rs` (`cbrt`), unchanged.
#[inline]
pub fn cbrt(x: f64) -> f64 {
    let (xm, xe) = frexp_i32(x.abs());
    if xe == 0 && (x == 0.0 || !x.is_finite()) {
        return x + x;
    }
    let u = (-0.145_263_899_385_486_37_f64).mul_add(xm, 0.784_932_344_976_639_2);
    let u = u.mul_add(xm, -1.834_692_774_836_130_8);
    let u = u.mul_add(xm, 2.446_931_225_635_344_4);
    let u = u.mul_add(xm, -2.114_994_941_673_713);
    let u = u.mul_add(xm, 1.508_191_937_815_849);
    let u = u.mul_add(xm, 0.354_895_765_043_919_84);
    let t2 = u * u * u;
    let ym =
        u * 2.0f64.mul_add(xm, t2) / 2.0f64.mul_add(t2, xm) * CBRT_FACTOR[(2 + xe % 3) as usize];
    scalbn(if x > 0.0 { ym } else { -ym }, xe / 3)
}

/// `Float.atanh` (extern `atanh`): glibc's `sysdeps/ieee754/dbl-64/e_atanh.c`
/// over `log1p` (`f64::ln_1p`, which calls glibc's `log1p`). Rust's
/// `f64::atanh` is a different formula.
///
/// Source: leanrs_rt `src/float.rs` (`atanh`), unchanged.
#[inline]
pub fn atanh(x: f64) -> f64 {
    let xa = x.abs();
    let t = if xa < 0.5 {
        if xa < f64::from_bits(0x3e30_0000_0000_0000) {
            return x; // |x| < 2^-28
        }
        let t = xa + xa;
        0.5 * (t + t * xa / (1.0 - xa)).ln_1p()
    } else if xa < 1.0 {
        0.5 * ((xa + xa) / (1.0 - xa)).ln_1p()
    } else if xa > 1.0 {
        return f64::NAN; // glibc: (x - x) / (x - x)
    } else if xa == 1.0 {
        return x / 0.0;
    } else {
        return x + x; // NaN
    };
    t.copysign(x)
}

macro_rules! f32_unary {
    ($($name:ident => $method:ident),* $(,)?) => {
        $(
            #[doc = concat!("`Float32` extern `", stringify!($name), "`: glibc's `",
                stringify!($name), "`, through `f32::", stringify!($method),
                "` on an operand the compiler cannot see.")]
            ///
            /// Source: leanrs_rt `src/libm.rs` (`unary!`), unchanged.
            #[inline]
            pub fn $name(x: f32) -> f32 {
                black_box(x).$method()
            }
        )*
    };
}

f32_unary! {
    acosf => acos, acoshf => acosh, asinf => asin, asinhf => asinh, atanf => atan, cosf => cos,
    coshf => cosh, expf => exp, exp2f => exp2, logf => ln, log10f => log10, log2f => log2,
    sinf => sin, sinhf => sinh, tanf => tan, tanhf => tanh,
}

macro_rules! f32_exact {
    ($($name:ident => $method:ident),* $(,)?) => {
        $(
            #[doc = concat!("`Float32` extern `", stringify!($name), "`, through `f32::",
                stringify!($method), "`.")]
            ///
            /// An exact operation (one correctly rounded result, or none to round), so a
            /// compile-time evaluation of a constant operand gives glibc's result and the
            /// operand needs no `black_box`; the call stays one instruction, as natively.
            ///
            /// Source: leanrs_rt `src/libm.rs` (`unary!`), adapted: no `black_box`.
            #[inline]
            pub fn $name(x: f32) -> f32 {
                x.$method()
            }
        )*
    };
}

f32_exact! {
    fabsf => abs, ceilf => ceil, floorf => floor, roundf => round, sqrtf => sqrt,
}

/// `Float32.atan2` (extern `atan2f`): glibc's `atan2f(y, x)`.
///
/// Source: leanrs_rt `src/libm.rs` (`atan2f`), unchanged.
#[inline]
pub fn atan2f(y: f32, x: f32) -> f32 {
    black_box(y).atan2(black_box(x))
}

/// `Float32.pow` (extern `powf`): glibc's `powf`; both operands are opaque, so
/// no constant exponent rewrites the call.
///
/// Source: leanrs_rt `src/libm.rs` (`powf`), unchanged.
#[inline]
pub fn powf(x: f32, y: f32) -> f32 {
    black_box(x).powf(black_box(y))
}

/// `Float32.cbrt` (extern `cbrtf`): glibc 2.39's
/// `sysdeps/ieee754/flt-32/s_cbrtf.c` as its aarch64 build computes it: the
/// polynomial in `double`, its two sums fused (`mul_add`), rounded to
/// `float`; `t2 = u * u * u` in `float`; the Newton step in `double`; `ldexpf`
/// on the widened value, exact there, rounded once. Rust's `f32::cbrt` is
/// correctly rounded and differs.
///
/// Source: leanrs_rt `src/float32.rs` (`cbrt`), unchanged.
#[inline]
pub fn cbrtf(x: f32) -> f32 {
    let x = black_box(x);
    let (xm, xe) = frexp_i32(f64::from(x.abs()));
    if xe == 0 && (x == 0.0 || !x.is_finite()) {
        return x + x;
    }
    let u = (-0.191_502_161_678_719_06_f64).mul_add(xm, 0.697_570_460_207_922_8);
    let u = u.mul_add(xm, 0.492_659_620_528_969_56) as f32;
    let t2 = f64::from(u * u * u);
    let ym = (f64::from(u) * 2.0f64.mul_add(xm, t2) / 2.0f64.mul_add(t2, xm)
        * CBRT_FACTOR[(2 + xe % 3) as usize]) as f32;
    let ym = if x > 0.0 { ym } else { -ym };
    scalbn(f64::from(ym), xe / 3) as f32
}

/// `Float32.atanh` (extern `atanhf`): glibc 2.39's
/// `sysdeps/ieee754/flt-32/e_atanhf.c` over `log1pf` (`f32::ln_1p`, which
/// calls glibc's `log1pf`). Rust's `f32::atanh` is a different formula.
///
/// Source: leanrs_rt `src/float32.rs` (`atanh`), unchanged.
#[inline]
pub fn atanhf(x: f32) -> f32 {
    let x = black_box(x);
    let xa = x.abs();
    let t = if xa < 0.5 {
        if xa < f32::from_bits(0x3180_0000) {
            return x; // |x| < 2^-28
        }
        let t = xa + xa;
        0.5 * (t + t * xa / (1.0 - xa)).ln_1p()
    } else if xa < 1.0 {
        0.5 * ((xa + xa) / (1.0 - xa)).ln_1p()
    } else if xa > 1.0 {
        return f32::NAN; // glibc: (x - x) / (x - x)
    } else if xa == 1.0 {
        return x / 0.0;
    } else {
        return x + x; // NaN
    };
    t.copysign(x)
}
