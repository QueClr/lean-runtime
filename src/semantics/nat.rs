//! Lean 4.34.0's `Nat` externs (`include/lean/lean.h`, `src/runtime/object.cpp`):
//! what each one does with a zero divisor, a truncating subtraction, an
//! exponent or a shift amount, in two layers.
//!
//! - **Word helpers** (`*_small`): the rule on two `u64` values, plain data
//!   in and out, `#[inline]`, for a translator's own fast path on its
//!   unboxed words.
//! - **Rules** on `Nat<B>`, a value that is either a word (`Small`) or a big
//!   number (`Big`) of the translator's `BigNat` backend. Each rule handles
//!   the two-words case inline and calls a `#[cold]` slow path for the rest,
//!   so a translator can call the rule for every case. The slow path never
//!   assumes a `Big` value is at least 2^64 (only that it is a value), and a
//!   result is `Small` only when the rule computed it on words; a translator
//!   normalizes every result with its own boundary (2^63 for both).
//!
//! Lean's limits (`semantics::panic::InternalPanic`): `Nat.pow` and
//! `Nat.shiftLeft` with an exponent of 2^32 or more end the process as
//! natively, in one function each (`pow_exponent`, `shiftl_amount`).
//! `Nat.shiftRight` has no limit: native panics when it shifts an operand of
//! 2^32 bits or more by 2^32 or more (LB-04); here it computes the
//! definition's result (owner, 2026-10-03).
//!
//! The `Nat` externs of 4.34.0 are `lean_nat_add`, `_sub`, `_mul`, `_div`,
//! `_div_exact`, `_mod`, `_pow`, `_gcd`, `_log2`, `_land`, `_lor`, `_lxor`,
//! `_shiftl`, `_shiftr`, `_dec_eq`, `_dec_lt`, `_dec_le` and `_pred`;
//! `Nat.repr` is Lean code that formats a value below 2^64 with
//! `lean_string_of_usize` (`write_decimal`). `Nat.lcm`, `Nat.sqrt` and the
//! other `Nat` functions are Lean code that both translators compile.
//!
//! Source: leanrs_rt `src/nat.rs` (the word fast paths, `gcd_u64`, the shape
//! of a fast path plus a cold slow path) and lean2rr's leanrt `src/nat.rs`
//! (the case split of each slow path over small and big operands, `gcd` of a
//! small and a big value), both checked against `object.cpp`.

use core::cmp::Ordering;
use core::fmt;

use super::bignum::BigNat;
use super::panic::InternalPanic;

/// A `Nat` as the rules see it: a word, or a big number of the backend `B`.
/// Either form may hold any value (see the module comment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Nat<B> {
    Small(u64),
    Big(B),
}

use Nat::{Big, Small};

impl<B: BigNat> Nat<B> {
    /// The value when it is below 2^64.
    #[inline]
    pub fn to_u64(&self) -> Option<u64> {
        match self {
            Small(x) => Some(*x),
            Big(b) => b.to_u64(),
        }
    }

    /// The value modulo 2^64 (`lean_uint64_of_nat`; the `UIntN.ofNat` rows
    /// of `semantics::uint` take it).
    #[inline]
    pub fn low_u64(&self) -> u64 {
        match self {
            Small(x) => *x,
            Big(b) => b.low_u64(),
        }
    }

    /// The value, or `u64::MAX` when it is 2^64 or more: an index or a size
    /// that no array reaches.
    #[inline]
    pub fn to_u64_saturating(&self) -> u64 {
        self.to_u64().unwrap_or(u64::MAX)
    }

    /// Whether the value is zero.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.to_u64() == Some(0)
    }

    /// The value as a big number.
    #[inline]
    fn into_big(self) -> B {
        match self {
            Small(x) => B::from_u64(x),
            Big(b) => b,
        }
    }
}

/// A sum or product of two words: a word when it fits.
#[inline]
fn of_u128<B: BigNat>(v: u128) -> Nat<B> {
    match u64::try_from(v) {
        Ok(x) => Small(x),
        Err(_) => Big(B::from_u128(v)),
    }
}

// ------------------------------------------------------------------ word helpers

/// `a + b` on two words (`lean_nat_add`'s scalar path, where both are below
/// 2^63 and the sum fits a word).
///
/// Source: leanrs_rt `src/nat.rs` (`add_ref`), widened to `u128` so that any
/// two `u64` values add.
#[inline]
pub fn add_small(a: u64, b: u64) -> u128 {
    a as u128 + b as u128
}

/// `a - b` truncated at zero (`lean_nat_sub`: `n1 < n2` gives 0).
///
/// Source: leanrs_rt `src/nat.rs` (`sub_ref`), unchanged.
#[inline]
pub fn sub_small(a: u64, b: u64) -> u64 {
    a.saturating_sub(b)
}

/// `a * b` (`lean_nat_mul`, which falls back to `lean_nat_overflow_mul`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`mul_wide`), unchanged.
#[inline]
pub fn mul_small(a: u64, b: u64) -> u128 {
    a as u128 * b as u128
}

/// `a / b` with `a / 0 = 0` (`lean_nat_div`).
///
/// Source: leanrs_rt `src/nat.rs` (`div_ref`), unchanged.
#[inline]
pub fn div_small(a: u64, b: u64) -> u64 {
    a.checked_div(b).unwrap_or(0)
}

/// `a % b` with `a % 0 = a` (`lean_nat_mod`).
///
/// Source: leanrs_rt `src/nat.rs` (`rem_ref`), unchanged.
#[inline]
pub fn mod_small(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        a % b
    }
}

/// `a ^ e` when it fits a word (`mpz_pow_ui` otherwise), with `0 ^ 0 = 1`.
/// `None` when the power is 2^64 or more. The exponent limit is
/// `pow_exponent`'s, checked before.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::pow`'s fast path), extended to every
/// exponent: a base of 0 or 1 never overflows.
#[inline]
pub fn pow_small(a: u64, e: u64) -> Option<u64> {
    match a {
        0 => Some((e == 0) as u64),
        1 => Some(1),
        _ => u32::try_from(e).ok().and_then(|e| a.checked_pow(e)),
    }
}

/// `gcd a b` with `gcd 0 b = b` (`lean_nat_gcd`): Stein's binary gcd.
///
/// Source: leanrs_rt `src/nat.rs` (`gcd_u64`), unchanged.
#[inline]
pub fn gcd_small(mut a: u64, mut b: u64) -> u64 {
    if a == 0 {
        return b;
    }
    if b == 0 {
        return a;
    }
    let shift = (a | b).trailing_zeros();
    a >>= a.trailing_zeros();
    loop {
        b >>= b.trailing_zeros();
        if a > b {
            core::mem::swap(&mut a, &mut b);
        }
        b -= a;
        if b == 0 {
            return a << shift;
        }
    }
}

/// `Nat.log2` (`lean_nat_log2`): the floor of the base-2 logarithm, with
/// `log2 0 = 0`. C halves the value in a loop; `ilog2` is the same number.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::log2`), unchanged.
#[inline]
pub fn log2_small(a: u64) -> u64 {
    a.checked_ilog2().map_or(0, u64::from)
}

/// `a <<< s` when it fits a word, else `None`. The shift limit is
/// `shiftl_amount`'s, checked before.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::shl`'s fast path), extended to
/// results up to 2^64 - 1.
#[inline]
pub fn shiftl_small(a: u64, s: u64) -> Option<u64> {
    if a == 0 {
        Some(0)
    } else if s < 64 && u64::from(a.leading_zeros()) >= s {
        Some(a << s)
    } else {
        None
    }
}

/// `a >>> s` (`lean_nat_shiftr`): 0 for a shift of 64 or more.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::shr`), unchanged.
#[inline]
pub fn shiftr_small(a: u64, s: u64) -> u64 {
    if s >= 64 {
        0
    } else {
        a >> s
    }
}

// ------------------------------------------------------------------ the limits kept

/// `Nat.pow`'s limit (`lean_nat_pow`): an exponent of 2^32 or more ends the
/// process with `INTERNAL PANIC: Nat.pow exponent is too big`, whatever the
/// base, 0 and 1 included. Otherwise the exponent as a `u32`.
///
/// Kept as native: the owner's decision lifted LB-04..06 only. Lifting this
/// one too is this function returning the exponent for every base whose
/// power is computable.
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_pow`'s first test), unchanged.
#[inline]
pub fn pow_exponent<B: BigNat>(e: &Nat<B>) -> Result<u32, InternalPanic> {
    match e.to_u64().map(u32::try_from) {
        Some(Ok(e)) => Ok(e),
        _ => Err(InternalPanic::NatPowExponent),
    }
}

/// `Nat.shiftLeft`'s limit (`lean_nat_shiftl`): a shift of 2^32 or more of a
/// nonzero value ends the process with `INTERNAL PANIC: Nat.shiftl exponent
/// is too big`. Zero shifted by anything is zero (C tests that first).
/// Otherwise the shift amount.
///
/// Kept as native, as `pow_exponent`.
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_shiftl`), unchanged.
#[inline]
pub fn shiftl_amount<B: BigNat>(s: &Nat<B>) -> Result<u64, InternalPanic> {
    match s.to_u64() {
        Some(s) if s <= u32::MAX as u64 => Ok(s),
        _ => Err(InternalPanic::NatShiftlExponent),
    }
}

// ------------------------------------------------------------------ the rules

/// `Nat.add` (`lean_nat_add`).
///
/// Source: leanrs_rt `src/nat.rs` (`add_ref`, `add_slow`) and lean2rr leanrt
/// `src/nat.rs` (`nat_add`), merged.
#[inline]
pub fn add<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_u128(add_small(x, y)),
        (a, b) => add_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn add_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_u128(add_small(x, y)),
        (Small(x), Big(y)) | (Big(y), Small(x)) => Big(y.add_u64(x)),
        (Big(x), Big(y)) => Big(x.add(y)),
    }
}

/// `Nat.succ` (`lean_nat_succ`, which compiled code uses for `n + 1`).
///
/// Source: lean2rr's `runtime/prelude.rr` (`lean_nat_succ`: `add` of 1).
#[inline]
pub fn succ<B: BigNat>(a: Nat<B>) -> Nat<B> {
    add(a, Small(1))
}

/// `Nat.sub` (`lean_nat_sub`): truncated at zero, `a - b = 0` when `b >= a`.
///
/// Source: leanrs_rt `src/nat.rs` (`sub_ref`, `sub_slow`) and lean2rr leanrt
/// `src/nat.rs` (`nat_sub`), merged.
#[inline]
pub fn sub<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(sub_small(x, y)),
        (a, b) => sub_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn sub_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(sub_small(x, y)),
        (Small(x), Big(y)) => Small(y.to_u64().map_or(0, |y| sub_small(x, y))),
        (Big(x), Small(y)) => {
            if x.compare_u64(y) == Ordering::Greater {
                Big(x.sub_u64(y))
            } else {
                Small(0)
            }
        }
        (Big(x), Big(y)) => {
            if x.compare(&y) == Ordering::Greater {
                Big(x.sub(y))
            } else {
                Small(0)
            }
        }
    }
}

/// `Nat.pred` (`lean_nat_pred`, `n - 1`): `pred 0 = 0`.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::pred`), through `sub` as `lean.h`.
#[inline]
pub fn pred<B: BigNat>(a: Nat<B>) -> Nat<B> {
    sub(a, Small(1))
}

/// `Nat.mul` (`lean_nat_mul`).
///
/// Source: leanrs_rt `src/nat.rs` (`mul_ref`, `mul_slow`) and lean2rr leanrt
/// `src/nat.rs` (`nat_mul`: a zero word times a big number is zero without a
/// big product), merged.
#[inline]
pub fn mul<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_u128(mul_small(x, y)),
        (a, b) => mul_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn mul_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_u128(mul_small(x, y)),
        (Small(0), Big(_)) | (Big(_), Small(0)) => Small(0),
        (Small(x), Big(y)) | (Big(y), Small(x)) => Big(y.mul_u64(x)),
        (Big(x), Big(y)) => Big(x.mul(y)),
    }
}

/// `Nat.div` (`lean_nat_div`): `a / 0 = 0`.
///
/// Source: leanrs_rt `src/nat.rs` (`div_ref`, `div_slow`) and lean2rr leanrt
/// `src/nat.rs` (`nat_div`), merged.
#[inline]
pub fn div<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(div_small(x, y)),
        (a, b) => div_slow(a, b, false),
    }
}

/// `Nat.divExact` (`lean_nat_div_exact`): `a / b` where `b` divides `a`
/// (the proof argument guarantees it); `a / 0 = 0` as in `lean.h`.
///
/// Source: new, from `lean.h` and `object.cpp` (`lean_nat_big_div_exact`).
#[inline]
pub fn div_exact<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(div_small(x, y)),
        (a, b) => div_slow(a, b, true),
    }
}

#[cold]
#[inline(never)]
fn div_slow<B: BigNat>(a: Nat<B>, b: Nat<B>, exact: bool) -> Nat<B> {
    if b.is_zero() {
        return Small(0);
    }
    match (a, b) {
        (Small(x), b) => Small(b.to_u64().map_or(0, |y| x / y)),
        (Big(x), Small(y)) => Big(x.div_u64(y)),
        (Big(x), Big(y)) => Big(if exact { x.div_exact(y) } else { x.div(y) }),
    }
}

/// `Nat.mod` (`lean_nat_mod`): `a % 0 = a`.
///
/// Source: leanrs_rt `src/nat.rs` (`rem_ref`, `rem_slow`) and lean2rr leanrt
/// `src/nat.rs` (`nat_mod`: a big value modulo a word is a word), merged.
#[inline]
pub fn rem<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(mod_small(x, y)),
        (a, b) => rem_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn rem_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    if b.is_zero() {
        return a;
    }
    match (a, b) {
        (Small(x), b) => Small(b.to_u64().map_or(x, |y| x % y)),
        (Big(x), Small(y)) => Small(x.rem_u64(y)),
        (Big(x), Big(y)) => Big(x.rem(y)),
    }
}

/// `Nat.pow` (`lean_nat_pow`): `a ^ e`, `0 ^ 0 = 1`; an exponent of 2^32 or
/// more is `pow_exponent`'s internal panic, returned for the caller to end
/// the process with.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::pow`, `pow_slow`) and lean2rr leanrt
/// `src/nat.rs` (`nat_pow`), merged.
#[inline]
pub fn pow<B: BigNat>(a: Nat<B>, e: Nat<B>) -> Result<Nat<B>, InternalPanic> {
    let e = pow_exponent(&e)?;
    if let Small(x) = a {
        if let Some(v) = pow_small(x, u64::from(e)) {
            return Ok(Small(v));
        }
    }
    Ok(pow_slow(a, e))
}

#[cold]
#[inline(never)]
fn pow_slow<B: BigNat>(a: Nat<B>, e: u32) -> Nat<B> {
    Big(a.into_big().pow(e))
}

/// `Nat.gcd` (`lean_nat_gcd`): `gcd 0 b = b`, `gcd a 0 = a`.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::gcd`) and lean2rr leanrt `src/nat.rs`
/// (`nat_gcd`: a word and a big value reduce to two words), merged.
#[inline]
pub fn gcd<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(gcd_small(x, y)),
        (a, b) => gcd_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn gcd_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(gcd_small(x, y)),
        (Small(0), Big(y)) | (Big(y), Small(0)) => Big(y),
        (Small(x), Big(y)) | (Big(y), Small(x)) => Small(gcd_small(x, y.rem_u64(x))),
        (Big(x), Big(y)) => Big(x.gcd(y)),
    }
}

/// `Nat.log2` (`lean_nat_log2`): `log2 0 = 0`; the result is always a word.
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::log2`, `log2_slow`), unchanged.
#[inline]
pub fn log2<B: BigNat>(a: &Nat<B>) -> u64 {
    match a {
        Small(x) => log2_small(*x),
        Big(b) => b.bit_len().saturating_sub(1),
    }
}

/// `Nat.land` (`lean_nat_land`): with a word operand the result is a word.
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_land`) and leanrs_rt
/// `src/nat.rs` (`land_slow`), merged.
#[inline]
pub fn land<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(x & y),
        (a, b) => land_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn land_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(x & y),
        (Small(x), Big(y)) | (Big(y), Small(x)) => Small(x & y.low_u64()),
        (Big(x), Big(y)) => Big(x.and(y)),
    }
}

/// `Nat.lor` (`lean_nat_lor`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_lor`), unchanged.
#[inline]
pub fn lor<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(x | y),
        (a, b) => lor_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn lor_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(x | y),
        (Small(x), Big(y)) | (Big(y), Small(x)) => Big(y.or_u64(x)),
        (Big(x), Big(y)) => Big(x.or(y)),
    }
}

/// `Nat.xor` (`lean_nat_lxor`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_xor`), unchanged.
#[inline]
pub fn lxor<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(x ^ y),
        (a, b) => lxor_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn lxor_slow<B: BigNat>(a: Nat<B>, b: Nat<B>) -> Nat<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(x ^ y),
        (Small(x), Big(y)) | (Big(y), Small(x)) => Big(y.xor_u64(x)),
        (Big(x), Big(y)) => Big(x.xor(y)),
    }
}

/// `Nat.shiftLeft` (`lean_nat_shiftl`): `a * 2^s`. Zero stays zero for any
/// `s`; otherwise a shift of 2^32 or more is `shiftl_amount`'s internal
/// panic, returned for the caller to end the process with.
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_shiftl`) and leanrs_rt
/// `src/nat.rs` (`Nat::shl`, `shl_slow`), merged.
#[inline]
pub fn shiftl<B: BigNat>(a: Nat<B>, s: Nat<B>) -> Result<Nat<B>, InternalPanic> {
    if a.is_zero() {
        return Ok(Small(0));
    }
    let s = shiftl_amount(&s)?;
    if let Small(x) = a {
        if let Some(v) = shiftl_small(x, s) {
            return Ok(Small(v));
        }
    }
    Ok(shiftl_slow(a, s))
}

#[cold]
#[inline(never)]
fn shiftl_slow<B: BigNat>(a: Nat<B>, s: u64) -> Nat<B> {
    Big(a.into_big().shl(s))
}

/// `Nat.shiftRight` (`lean_nat_shiftr`): `a / 2^s`, 0 once `s` reaches the
/// bit length. No limit (LB-04 lifted): where native ends with `INTERNAL
/// PANIC: Nat.shiftr exponent is too big` (an operand of 2^32 bits or more
/// shifted by 2^32 or more, `lean_nat_big_shiftr`), this computes the
/// quotient.
///
/// Source: lean2rr leanrt `src/nat.rs` (`nat_shiftr`) and leanrs_rt
/// `src/nat.rs` (`Nat::shr`, `shr_slow`), merged, without native's limit.
#[inline]
pub fn shiftr<B: BigNat>(a: Nat<B>, s: Nat<B>) -> Nat<B> {
    match (a, s) {
        (Small(x), Small(s)) => Small(shiftr_small(x, s)),
        (a, s) => shiftr_slow(a, s),
    }
}

#[cold]
#[inline(never)]
fn shiftr_slow<B: BigNat>(a: Nat<B>, s: Nat<B>) -> Nat<B> {
    // A shift of 2^64 or more clears every value (none has that many bits).
    let Some(s) = s.to_u64() else {
        return Small(0);
    };
    match a {
        Small(x) => Small(shiftr_small(x, s)),
        Big(x) if s >= x.bit_len() => Small(0),
        Big(x) => Big(x.shr(s)),
    }
}

/// The order of two values: a word below every big value of 2^64 or more,
/// two big values by the backend (`lean_nat_big_eq`, `_le`, `_lt`).
///
/// Source: leanrs_rt `src/nat.rs` (`cmp_slow`), adapted to big values that
/// may be below 2^64.
#[inline]
pub fn compare<B: BigNat>(a: &Nat<B>, b: &Nat<B>) -> Ordering {
    match (a, b) {
        (Small(x), Small(y)) => x.cmp(y),
        (a, b) => compare_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn compare_slow<B: BigNat>(a: &Nat<B>, b: &Nat<B>) -> Ordering {
    match (a, b) {
        (Small(x), Small(y)) => x.cmp(y),
        (Small(x), Big(y)) => y.compare_u64(*x).reverse(),
        (Big(x), Small(y)) => x.compare_u64(*y),
        (Big(x), Big(y)) => x.compare(y),
    }
}

/// `Nat.decEq` and `Nat.beq` (`lean_nat_dec_eq`).
///
/// Source: leanrs_rt `src/nat.rs` (`PartialEq for Nat`), through `compare`.
#[inline]
pub fn dec_eq<B: BigNat>(a: &Nat<B>, b: &Nat<B>) -> bool {
    compare(a, b) == Ordering::Equal
}

/// `Nat.decLt` (`lean_nat_dec_lt`).
///
/// Source: leanrs_rt `src/nat.rs` (`lt_ref`), through `compare`.
#[inline]
pub fn dec_lt<B: BigNat>(a: &Nat<B>, b: &Nat<B>) -> bool {
    compare(a, b) == Ordering::Less
}

/// `Nat.decLe` and `Nat.ble` (`lean_nat_dec_le`).
///
/// Source: leanrs_rt `src/nat.rs` (`le_ref`), through `compare`.
#[inline]
pub fn dec_le<B: BigNat>(a: &Nat<B>, b: &Nat<B>) -> bool {
    compare(a, b) != Ordering::Greater
}

/// `Nat.repr`'s text: the decimal digits (`Nat.reprFast`: below 2^64
/// `lean_string_of_usize`, above it `Nat.toDigits`, which gives the same
/// digits as GMP's `mpz_get_str`).
///
/// Source: leanrs_rt `src/nat.rs` (`Nat::repr`) and lean2rr leanrt
/// `src/nat.rs` (`nat_repr`), writing into the caller's buffer.
pub fn write_decimal<B: BigNat, W: fmt::Write + ?Sized>(a: &Nat<B>, out: &mut W) -> fmt::Result {
    match a {
        Small(x) => out.write_str(super::repr::decimal_u64(*x, &mut [0; 20])),
        Big(b) => b.write_decimal(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word helpers on values of `tests/cases/nat` (the rules themselves
    /// run on every row in `tests/rows2.rs`).
    #[test]
    fn word_helpers() {
        assert_eq!(add_small(u64::MAX, u64::MAX), (1 << 65) - 2);
        assert_eq!(sub_small(3, 5), 0);
        assert_eq!(sub_small(5, 5), 0);
        assert_eq!(mul_small(u64::MAX, u64::MAX), (u64::MAX as u128).pow(2));
        assert_eq!((div_small(7, 0), mod_small(7, 0)), (0, 7));
        assert_eq!((div_small(0, 0), mod_small(0, 0)), (0, 0));
        assert_eq!(pow_small(0, 0), Some(1));
        assert_eq!(pow_small(0, 1 << 40), Some(0));
        assert_eq!(pow_small(1, u64::MAX), Some(1));
        assert_eq!(pow_small(2, 63), Some(1 << 63));
        assert_eq!(pow_small(2, 64), None);
        assert_eq!(pow_small(3, 40), Some(12157665459056928801));
        assert_eq!(pow_small(3, 41), None);
        assert_eq!(
            (gcd_small(0, 0), gcd_small(0, 5), gcd_small(12, 18)),
            (0, 5, 6)
        );
        assert_eq!(
            (log2_small(0), log2_small(1), log2_small(u64::MAX)),
            (0, 0, 63)
        );
        assert_eq!(shiftl_small(1, 63), Some(1 << 63));
        assert_eq!(shiftl_small(1, 64), None);
        assert_eq!(shiftl_small(3, 63), None);
        assert_eq!(shiftl_small(0, 1000), Some(0));
        assert_eq!(
            (shiftr_small(u64::MAX, 63), shiftr_small(u64::MAX, 64)),
            (1, 0)
        );
    }
}
