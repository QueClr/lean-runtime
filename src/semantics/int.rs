//! Lean 4.34.0's `Int` externs (`include/lean/lean.h`, `src/runtime/object.cpp`,
//! `src/runtime/mpz.cpp`): the division conventions, the zero divisors and
//! the conversions to and from `Nat`, in the two layers of `semantics::nat`:
//! word helpers on `i64` (`*_small`) and rules on `Int<B>` over a `BigInt`
//! backend.
//!
//! A `Small` value is any `i64`. Lean's C keeps only the `int32` range in a
//! word (`LEAN_MIN_SMALL_INT..=LEAN_MAX_SMALL_INT`) and computes the word
//! cases in `int64_t`; lean2rr keeps the same range, leanrs the `i64` range.
//! The helpers compute in `i128`, so they are exact for any two `i64`s, and
//! each translator normalizes results with its own boundary.
//!
//! The conventions (`Init/Data/Int/DivMod/Basic.lean` and `lean.h`):
//! - `Int.tdiv`/`Int.tmod` (`lean_int_div`, `lean_int_mod`): C's `/` and
//!   `%`, the quotient rounded toward zero and the remainder with the
//!   dividend's sign;
//! - `Int.ediv`/`Int.emod` (`/` and `%`, `lean_int_ediv`, `lean_int_emod`):
//!   the remainder in `[0, |d|)`, the quotient adjusted to match;
//! - a zero divisor gives the quotient 0 and the remainder the dividend;
//! - `Int.divExact` (`lean_int_div_exact`): the quotient of an exact
//!   division, 0 for a zero divisor.
//!
//! `Int.fdiv`, `Int.fmod`, `Int.bmod`, `Int.bdiv`, `Int.pow`, `Int.toNat`, the
//! bitwise operations and shifts of `Int` are Lean code in 4.34.0 (no
//! `@[extern]`), which both translators compile, so they are not here.
//!
//! Source: leanrs_rt `src/int.rs` (the `i64` small paths with `checked_*`,
//! `i64::MIN / -1`, the Euclidean fix-ups on a truncating pair) and lean2rr
//! leanrt `src/nat.rs` (the `Int` slow paths' case split, the `i128` small
//! arithmetic), both checked against `lean.h` and `object.cpp`.

use core::cmp::Ordering;
use core::fmt;

use super::bignum::{BigInt, BigNat};
use super::nat::Nat;

/// An `Int` as the rules see it: a word, or a big number of the backend `B`.
/// Either form may hold any value (see the module comment of
/// `semantics::nat`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Int<B> {
    Small(i64),
    Big(B),
}

use Int::{Big, Small};

impl<B: BigInt> Int<B> {
    /// The value when it is in the `i64` range.
    #[inline]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Small(x) => Some(*x),
            Big(b) => b.to_i64(),
        }
    }

    /// The value modulo 2^64 in two's complement (`lean_int64_of_int`; the
    /// `IntN.ofInt` rows of `semantics::sint` take it).
    #[inline]
    pub fn low_u64(&self) -> u64 {
        match self {
            Small(x) => *x as u64,
            Big(b) => b.low_u64(),
        }
    }

    /// Whether the value is zero.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.to_i64() == Some(0)
    }

    /// Whether the value is below zero.
    #[inline]
    pub fn is_neg(&self) -> bool {
        match self {
            Small(x) => *x < 0,
            Big(b) => b.is_neg(),
        }
    }

    #[inline]
    fn into_big(self) -> B {
        match self {
            Small(x) => B::from_i64(x),
            Big(b) => b,
        }
    }
}

/// A result of word arithmetic: a word when it fits `i64`.
#[inline]
fn of_i128<B: BigInt>(v: i128) -> Int<B> {
    match i64::try_from(v) {
        Ok(x) => Small(x),
        Err(_) => Big(B::from_i128(v)),
    }
}

// ------------------------------------------------------------------ word helpers

/// `a + b` (`lean_int_add`'s scalar path).
///
/// Source: lean2rr leanrt `src/nat.rs` (`int_add`), unchanged.
#[inline]
pub fn add_small(a: i64, b: i64) -> i128 {
    a as i128 + b as i128
}

/// `a - b` (`lean_int_sub`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`int_sub`), unchanged.
#[inline]
pub fn sub_small(a: i64, b: i64) -> i128 {
    a as i128 - b as i128
}

/// `a * b` (`lean_int_mul`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`int_mul`), unchanged.
#[inline]
pub fn mul_small(a: i64, b: i64) -> i128 {
    a as i128 * b as i128
}

/// `-a` (`lean_int_neg`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`int_neg`), unchanged.
#[inline]
pub fn neg_small(a: i64) -> i128 {
    -(a as i128)
}

/// The quotient of `i64::MIN / -1`, the one `i64` division that overflows.
const MIN_DIV_NEG_ONE: i128 = 1 << 63;

/// `Int.tdiv` (`lean_int_div`): rounded toward zero, `a / 0 = 0`;
/// `i64::MIN / -1` is 2^63. An `i64` division (one `sdiv`), never an
/// `i128` one, which is a library call.
///
/// Source: leanrs_rt `src/int.rs` (`Int::tdiv`), returning `i128`.
#[inline]
pub fn tdiv_small(a: i64, b: i64) -> i128 {
    if b == 0 {
        0
    } else {
        a.checked_div(b).map_or(MIN_DIV_NEG_ONE, i128::from)
    }
}

/// `Int.tmod` (`lean_int_mod`): the remainder with the dividend's sign,
/// `a % 0 = a`; `i64::MIN % -1` is 0.
///
/// Source: leanrs_rt `src/int.rs` (`Int::tmod`), unchanged.
#[inline]
pub fn tmod_small(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        a.checked_rem(b).unwrap_or(0)
    }
}

/// `Int.ediv` (`lean_int_ediv`): the quotient whose remainder is in
/// `[0, |b|)`, `a / 0 = 0`. As `lean.h`: the truncated quotient, minus one
/// when the remainder is negative and `b > 0`, plus one when `b < 0`. In
/// `i64` (no adjusted quotient overflows: a negative remainder needs
/// `|b| >= 2`), with `i64::MIN / -1` (remainder 0) answered apart.
///
/// Source: leanrs_rt `src/int.rs` (`Int::ediv`), returning `i128`; the
/// rule is lean2rr leanrt `src/nat.rs`'s `int_ediv`.
#[inline]
pub fn ediv_small(a: i64, b: i64) -> i128 {
    if b == 0 {
        return 0;
    }
    let (Some(q), Some(r)) = (a.checked_div(b), a.checked_rem(b)) else {
        return MIN_DIV_NEG_ONE;
    };
    i128::from(if r < 0 {
        if b > 0 {
            q - 1
        } else {
            q + 1
        }
    } else {
        q
    })
}

/// `Int.emod` (`lean_int_emod`): the remainder in `[0, |b|)`, `a % 0 = a`.
/// In `i64`: a negative remainder `r` becomes `r + b` or `r - b`, both in
/// `(0, |b|)`, so neither overflows, `b = i64::MIN` included.
///
/// Source: leanrs_rt `src/int.rs` (`Int::emod`); the rule is `lean.h`'s.
#[inline]
pub fn emod_small(a: i64, b: i64) -> i64 {
    if b == 0 {
        return a;
    }
    // `None` only for `i64::MIN % -1`, whose remainder is 0.
    let r = a.checked_rem(b).unwrap_or(0);
    if r < 0 {
        if b > 0 {
            r + b
        } else {
            r - b
        }
    } else {
        r
    }
}

// ------------------------------------------------------------------ the rules

/// `Int.neg` (`lean_int_neg`).
///
/// Source: lean2rr leanrt `src/nat.rs` (`int_neg`) and leanrs_rt
/// `src/int.rs` (`neg_ref`), merged.
#[inline]
pub fn neg<B: BigInt>(a: Int<B>) -> Int<B> {
    match a {
        Small(x) => of_i128(neg_small(x)),
        Big(b) => neg_slow(b),
    }
}

#[cold]
#[inline(never)]
fn neg_slow<B: BigInt>(b: B) -> Int<B> {
    Big(b.neg())
}

macro_rules! ring_op {
    ($(#[$doc:meta])* $name:ident, $slow:ident, $small:ident, $method:ident) => {
        $(#[$doc])*
        #[inline]
        pub fn $name<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
            match (a, b) {
                (Small(x), Small(y)) => of_i128($small(x, y)),
                (a, b) => $slow(a, b),
            }
        }

        #[cold]
        #[inline(never)]
        fn $slow<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
            match (a, b) {
                (Small(x), Small(y)) => of_i128($small(x, y)),
                (a, b) => Big(a.into_big().$method(b.into_big())),
            }
        }
    };
}

ring_op!(
    /// `Int.add` (`lean_int_add`).
    ///
    /// Source: lean2rr leanrt `src/nat.rs` (`int_add`) and leanrs_rt
    /// `src/int.rs` (`add_ref`, `add_slow`), merged.
    add,
    add_slow,
    add_small,
    add
);
ring_op!(
    /// `Int.sub` (`lean_int_sub`).
    ///
    /// Source: lean2rr leanrt `src/nat.rs` (`int_sub`) and leanrs_rt
    /// `src/int.rs` (`sub_ref`, `sub_slow`), merged.
    sub,
    sub_slow,
    sub_small,
    sub
);
ring_op!(
    /// `Int.mul` (`lean_int_mul`).
    ///
    /// Source: lean2rr leanrt `src/nat.rs` (`int_mul`) and leanrs_rt
    /// `src/int.rs` (`mul_ref`, `mul_slow`), merged.
    mul,
    mul_slow,
    mul_small,
    mul
);

/// Which division a slow path computes.
#[derive(Clone, Copy)]
enum Div {
    T,
    TMod,
    E,
    EMod,
    Exact,
}

#[cold]
#[inline(never)]
fn div_slow<B: BigInt>(a: Int<B>, b: Int<B>, k: Div) -> Int<B> {
    if b.is_zero() {
        // `lean_int_big_div` and friends: `a / 0 = 0`, `a % 0 = a`.
        return match k {
            Div::TMod | Div::EMod => a,
            Div::T | Div::E | Div::Exact => Small(0),
        };
    }
    if let (Small(x), Small(y)) = (&a, &b) {
        let (x, y) = (*x, *y);
        return match k {
            Div::T | Div::Exact => of_i128(tdiv_small(x, y)),
            Div::TMod => Small(tmod_small(x, y)),
            Div::E => of_i128(ediv_small(x, y)),
            Div::EMod => Small(emod_small(x, y)),
        };
    }
    let (a, b) = (a.into_big(), b.into_big());
    Big(match k {
        Div::T => a.tdiv(b),
        Div::TMod => a.tmod(b),
        Div::E => a.ediv(b),
        Div::EMod => a.emod(b),
        Div::Exact => a.div_exact(b),
    })
}

/// `Int.tdiv` (`lean_int_div`): rounded toward zero, `a / 0 = 0`.
///
/// Source: leanrs_rt `src/int.rs` (`Int::tdiv`, `tdiv_slow`) and lean2rr
/// leanrt `src/nat.rs` (`int_div`), merged.
#[inline]
pub fn tdiv<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_i128(tdiv_small(x, y)),
        (a, b) => div_slow(a, b, Div::T),
    }
}

/// `Int.tmod` (`lean_int_mod`): the dividend's sign, `a % 0 = a`.
///
/// Source: leanrs_rt `src/int.rs` (`Int::tmod`, `tmod_slow`) and lean2rr
/// leanrt `src/nat.rs` (`int_mod`), merged.
#[inline]
pub fn tmod<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(tmod_small(x, y)),
        (a, b) => div_slow(a, b, Div::TMod),
    }
}

/// `Int.ediv`, Lean's `/` (`lean_int_ediv`): `a / 0 = 0`.
///
/// Source: leanrs_rt `src/int.rs` (`Int::ediv`, `ediv_slow`) and lean2rr
/// leanrt `src/nat.rs` (`int_ediv`), merged.
#[inline]
pub fn ediv<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_i128(ediv_small(x, y)),
        (a, b) => div_slow(a, b, Div::E),
    }
}

/// `Int.emod`, Lean's `%` (`lean_int_emod`): in `[0, |b|)`, `a % 0 = a`.
///
/// Source: leanrs_rt `src/int.rs` (`Int::emod`, `emod_slow`) and lean2rr
/// leanrt `src/nat.rs` (`int_emod`), merged.
#[inline]
pub fn emod<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
    match (a, b) {
        (Small(x), Small(y)) => Small(emod_small(x, y)),
        (a, b) => div_slow(a, b, Div::EMod),
    }
}

/// `Int.divExact` (`lean_int_div_exact`): the quotient of an exact division
/// (the proof argument guarantees `b ∣ a`), 0 for a zero divisor.
///
/// Source: leanrs_rt `src/int.rs` (`Int::div_exact`), with `BigInt::div_exact`
/// for the big case as `object.cpp`'s `mpz::divexact`.
#[inline]
pub fn div_exact<B: BigInt>(a: Int<B>, b: Int<B>) -> Int<B> {
    match (a, b) {
        (Small(x), Small(y)) => of_i128(tdiv_small(x, y)),
        (a, b) => div_slow(a, b, Div::Exact),
    }
}

/// The order of two values (`lean_int_big_eq`, `_le`, `_lt`).
///
/// Source: leanrs_rt `src/int.rs` (`Ord for Int`), adapted to big values that
/// may be in the `i64` range.
#[inline]
pub fn compare<B: BigInt>(a: &Int<B>, b: &Int<B>) -> Ordering {
    match (a, b) {
        (Small(x), Small(y)) => x.cmp(y),
        (a, b) => compare_slow(a, b),
    }
}

#[cold]
#[inline(never)]
fn compare_slow<B: BigInt>(a: &Int<B>, b: &Int<B>) -> Ordering {
    // A big value outside the `i64` range is beyond every word, on its side
    // of zero.
    let beyond = |b: &B| {
        if b.is_neg() {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    };
    match (a, b) {
        (Small(x), Small(y)) => x.cmp(y),
        (Small(x), Big(y)) => y
            .to_i64()
            .map_or_else(|| beyond(y).reverse(), |y| x.cmp(&y)),
        (Big(x), Small(y)) => x.to_i64().map_or_else(|| beyond(x), |x| x.cmp(y)),
        (Big(x), Big(y)) => x.compare(y),
    }
}

/// `Int.decEq` (`lean_int_dec_eq`).
#[inline]
pub fn dec_eq<B: BigInt>(a: &Int<B>, b: &Int<B>) -> bool {
    compare(a, b) == Ordering::Equal
}

/// `Int.decLt` (`lean_int_dec_lt`).
#[inline]
pub fn dec_lt<B: BigInt>(a: &Int<B>, b: &Int<B>) -> bool {
    compare(a, b) == Ordering::Less
}

/// `Int.decLe` (`lean_int_dec_le`).
#[inline]
pub fn dec_le<B: BigInt>(a: &Int<B>, b: &Int<B>) -> bool {
    compare(a, b) != Ordering::Greater
}

/// `Int.decNonneg` (`lean_int_dec_nonneg`), behind `0 ≤ a` in compiled
/// `Int` pattern matches.
#[inline]
pub fn dec_nonneg<B: BigInt>(a: &Int<B>) -> bool {
    !a.is_neg()
}

/// `Int.ofNat` (`lean_nat_to_int`).
///
/// Source: leanrs_rt `src/int.rs` (`Int::from_nat`) and lean2rr leanrt
/// `src/nat.rs` (`nat_to_int`), merged.
#[inline]
pub fn of_nat<B: BigInt>(n: Nat<B::Nat>) -> Int<B> {
    match n {
        Nat::Small(x) if x <= i64::MAX as u64 => Small(x as i64),
        n => of_nat_slow(n),
    }
}

#[cold]
#[inline(never)]
fn of_nat_slow<B: BigInt>(n: Nat<B::Nat>) -> Int<B> {
    match n {
        Nat::Small(x) => match i64::try_from(x) {
            Ok(x) => Small(x),
            Err(_) => Big(B::from_nat(<B::Nat as BigNat>::from_u64(x))),
        },
        Nat::Big(n) => Big(B::from_nat(n)),
    }
}

/// `Int.negSucc n`, that is `-(n + 1)` (`lean_int_neg_succ_of_nat`).
///
/// Source: leanrs_rt `src/int.rs` (`Int::neg_succ`) and lean2rr leanrt
/// `src/nat.rs` (`nat_neg_succ`), merged.
#[inline]
pub fn neg_succ_of_nat<B: BigInt>(n: Nat<B::Nat>) -> Int<B> {
    match n {
        Nat::Small(x) => of_i128(-(x as i128) - 1),
        Nat::Big(n) => neg_succ_slow(n),
    }
}

#[cold]
#[inline(never)]
fn neg_succ_slow<B: BigInt>(n: B::Nat) -> Int<B> {
    Big(B::from_nat(n.add_u64(1)).neg())
}

/// `Int.natAbs` (`lean_nat_abs`): `|a|` as a `Nat`; `natAbs i64::MIN` is 2^63.
///
/// Source: leanrs_rt `src/int.rs` (`Int::nat_abs`: `unsigned_abs`, never
/// `wrapping_abs`) and lean2rr leanrt `src/nat.rs` (`int_nat_abs`), merged.
#[inline]
pub fn nat_abs<B: BigInt>(a: Int<B>) -> Nat<B::Nat> {
    match a {
        Small(x) => Nat::Small(x.unsigned_abs()),
        Big(b) => nat_abs_slow(b),
    }
}

#[cold]
#[inline(never)]
fn nat_abs_slow<B: BigInt>(b: B) -> Nat<B::Nat> {
    Nat::Big(b.nat_abs())
}

/// `Int.repr`'s text (`Init/Data/Int/Repr.lean`, Lean code that both
/// translators replace): the decimal digits with a leading `-` for a
/// negative value.
///
/// Source: lean2rr leanrt `src/nat.rs` (`int_repr`), writing into the
/// caller's buffer.
pub fn write_decimal<B: BigInt, W: fmt::Write + ?Sized>(a: &Int<B>, out: &mut W) -> fmt::Result {
    match a {
        Small(x) => {
            if *x < 0 {
                out.write_str("-")?;
            }
            out.write_str(super::repr::decimal_u64(x.unsigned_abs(), &mut [0; 20]))
        }
        Big(b) => b.write_decimal(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word helpers on values of `tests/cases/int`, `i64::MIN` included
    /// (the rules run on every row in `tests/rows2.rs`).
    #[test]
    fn word_helpers() {
        let min = i64::MIN;
        assert_eq!(tdiv_small(min, -1), 1 << 63);
        assert_eq!(tmod_small(min, -1), 0);
        assert_eq!(ediv_small(min, -1), 1 << 63);
        assert_eq!(emod_small(min, -1), 0);
        assert_eq!((tdiv_small(5, 0), tmod_small(5, 0)), (0, 5));
        assert_eq!((ediv_small(-5, 0), emod_small(-5, 0)), (0, -5));
        // Init/Data/Int/DivMod/Basic.lean's examples
        assert_eq!((ediv_small(-12, 7), emod_small(-12, 7)), (-2, 2));
        assert_eq!((ediv_small(-12, -7), emod_small(-12, -7)), (2, 2));
        assert_eq!((tdiv_small(-12, 7), tmod_small(-12, 7)), (-1, -5));
        assert_eq!((tdiv_small(-12, -7), tmod_small(-12, -7)), (1, -5));
        assert_eq!(emod_small(-1, min), i64::MAX);
        assert_eq!(ediv_small(-1, min), 1);
        assert_eq!(neg_small(min), 1 << 63);
        assert_eq!(mul_small(min, min), 1 << 126);
    }
}
