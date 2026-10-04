//! The big-number traits (decision Q4): the arithmetic on numbers that do not
//! fit a machine word, which each translator implements with its own library
//! (lean2rr with GMP's `mpn`/`mpz` functions on its one-block numbers, leanrs
//! with malachite), and on which `semantics::nat` and `semantics::int` state
//! Lean's rules.
//!
//! The split follows Lean's C runtime: `lean.h` and `object.cpp` decide what
//! each `Nat`/`Int` extern does with a zero divisor, a truncating
//! subtraction, an exponent or a shift amount, and which representation a
//! result gets; GMP (`mpz.cpp`) only computes. Here the rules are generic
//! functions over these traits, and the traits are the computing part.
//!
//! # Contract
//!
//! - A `BigNat` value is a natural number and a `BigInt` value an integer,
//!   with no size limit of their own and no normalization requirement: a
//!   value that would fit a word is still a valid `BigNat`/`BigInt`. The rules
//!   never assume a big operand is big; a translator normalizes the results
//!   with its own small-value boundary (both use 2^63 for `Nat`; lean2rr the
//!   `int32` range and leanrs the `i64` range for `Int`).
//! - Operations consume their operands, so an implementation can compute in
//!   place into a uniquely owned operand (lean2rr's `big.rs` does) or clone
//!   out of a shared one (leanrs's `Rc<Natural>`).
//! - A method's precondition (a nonzero divisor, `self >= o` for `sub`) is
//!   always established by the rule that calls it; implementations may
//!   assume it.
//! - The provided methods implement Lean's conventions in terms of the
//!   required ones. An implementation may override them with a faster
//!   equivalent (lean2rr's fused `mpn` Euclidean division, say); the rows in
//!   `tests/cases/{nat,int}` are what equivalent means.
//!
//! # Backend limits
//!
//! The rules lift Lean's size caps where a value is computable (owner,
//! 2026-10-03: LB-04, LB-06, LB-11 and LB-12 compute the definition's
//! result, and the LB-05 requirement to reproduce GMP's limb cap is
//! withdrawn) up to each backend's `BigNat::MAX_BITS`: above it, every rule
//! whose result size is known before computing it ends at once
//! (`nat::check_result_bits`, `INTERNAL PANIC: out of memory` or native's
//! exponent message), before calling the backend. A backend still stops
//! where its library stops in the operations whose size the rules do not
//! test, and where the machine has no memory for a result below
//! `MAX_BITS`:
//! - **GMP.** `mpz_t` sizes are `int`s: an `mpz_*` function whose allocation
//!   request exceeds `INT_MAX` limbs (2^31 - 1 limbs, 16 GiB) calls
//!   `__gmp_overflow_in_mpz`. In GMP 6.3.0, the version Lean 4.34.0 links
//!   (`lib/libgmp.a` of the toolchain), that raises `SIGFPE` with no message
//!   (`errno.c`: `__gmp_exception` raises `SIGFPE`, then `abort`s), so the
//!   process dies with status 136 (128 + 8); native Lean does so for
//!   `(2^62)^(2^32 - 1)`, whose `mpz_n_pow_ui` asks for about 2^32 limbs at
//!   once (LB-05). With `MAX_BITS` at most that cap, the rules never ask
//!   GMP for such a result: lean2rr's `big.rs` routes `pow` (`mpz_pow_ui`),
//!   `gcd` (`mpz_gcd`, no larger than its operands) and the decimal
//!   conversions (`mpz_get_str`, `mpz_set_str`) through `mpz`, and the rest
//!   through `mpn` functions on its own blocks, whose cap is `MAX_LIMBS`
//!   (`i32::MAX` limbs, then `INTERNAL PANIC: out of memory`).
//! - **malachite** has no limit below the address space; an allocation
//!   failure is Rust's allocation-failure abort. Its `pow` sizes the result
//!   as `bit_len * e` in 64 bits, which the size test keeps from wrapping
//!   (review RS2-01: `3^(2^63)` gave 1).
//!
//! Source: new (decision Q4). The method set is what lean2rr's leanrt
//! `src/big.rs` and leanrs_rt `src/nat.rs`/`src/int.rs` already compute
//! (`nat_add_u64`, `nat_mod_u64`, `int_tdiv`, `significant_bits`, ...), so
//! each implementation is a thin layer over existing code.

use core::cmp::Ordering;
use core::fmt;

/// A natural number of any size: the slow-path arithmetic of `Nat`.
pub trait BigNat: Sized {
    /// The largest result, in bits, the backend computes for any operation
    /// the rules ask of it. Every rule whose result size is known before
    /// computing it (`nat::add`, `mul`, `pow`, `shiftl`, ...; `int::add`,
    /// `sub`, `mul`, ...) tests its size against this first
    /// (`nat::check_result_bits`) and ends with `INTERNAL PANIC: out of
    /// memory` (or native's exponent message) above it, so no backend is
    /// asked for a result it cannot hold, and every backend ends the same way
    /// at its own limit (review RS2-02).
    ///
    /// Requirements: at least 2^33, so that the rows of
    /// `tests/cases/nat` run (their largest result has 2^32 + 65 bits),
    /// and below 2^64. A backend whose allocations reserve more than the
    /// result (GMP's `mpz_add` reserves one limb for a carry) states its
    /// limit minus that margin. The rules test upper bounds for `add`,
    /// `mul` and non-power-of-two `pow` (`nat`'s module doc), so a result
    /// close to `MAX_BITS` may be refused although it would fit.
    ///
    /// lean2rr's GMP backend: its block's limb cap, `i32::MAX` limbs
    /// (`big.rs` `MAX_LIMBS`, GMP's own `int` sizes), times 64, less a limb
    /// is a placeholder (the benches' `ColdBig` uses it). The value is set
    /// when lean2rr adopts the rules, with a margin taken from the sizes
    /// GMP's `mpz_pow_ui` reallocates to (`mpz/n_pow_ui.c`, its `ralloc`
    /// estimate and `MPZ_NEWALLOC`; review D3). leanrs's malachite backend
    /// picks its own.
    const MAX_BITS: u64;

    /// The value `v`.
    fn from_u64(v: u64) -> Self;

    /// The value `v` (a product of two words, a sum that carried).
    fn from_u128(v: u128) -> Self {
        let lo = Self::from_u64(v as u64);
        match (v >> 64) as u64 {
            0 => lo,
            hi => Self::from_u64(hi).shl(64).or_u64(v as u64),
        }
    }

    /// The value when it is below 2^64.
    fn to_u64(&self) -> Option<u64>;

    /// The value modulo 2^64 (`mpz::mod64`, the `UIntN.ofNat` rows).
    fn low_u64(&self) -> u64;

    /// The number of significant bits: 0 for zero, `log2 + 1` otherwise
    /// (`mpz_sizeinbase(x, 2)` for a nonzero `x`).
    fn bit_len(&self) -> u64;

    /// The order of two values (`mpz_cmp`).
    fn compare(&self, o: &Self) -> Ordering;

    /// The order of the value and `o` (`mpz_cmp_ui`).
    fn compare_u64(&self, o: u64) -> Ordering {
        match self.to_u64() {
            Some(v) => v.cmp(&o),
            None => Ordering::Greater,
        }
    }

    /// `self + o`, where `max(bit_len) + 1 <= MAX_BITS`.
    fn add(self, o: Self) -> Self;

    /// `self + o`, as `add`.
    fn add_u64(self, o: u64) -> Self;

    /// `self - o`, where `self >= o` (the rule truncates first).
    fn sub(self, o: Self) -> Self;

    /// `self - o`, where `self >= o`.
    fn sub_u64(self, o: u64) -> Self;

    /// `self * o`, where the sum of the bit lengths is at most `MAX_BITS`.
    fn mul(self, o: Self) -> Self;

    /// `self * o`, as `mul`.
    fn mul_u64(self, o: u64) -> Self;

    /// `self / o` rounded down, where `o != 0` (`mpz_tdiv_q`).
    fn div(self, o: Self) -> Self;

    /// `self % o`, where `o != 0` (`mpz_tdiv_r`).
    fn rem(self, o: Self) -> Self;

    /// `self / o`, where `o != 0`.
    fn div_u64(self, o: u64) -> Self;

    /// `self % o`, where `o != 0`.
    fn rem_u64(&self, o: u64) -> u64;

    /// `self / o` where `o` divides `self` and `o != 0` (`mpz_divexact`,
    /// faster than a division when the backend has it).
    fn div_exact(self, o: Self) -> Self {
        self.div(o)
    }

    /// `self & o`.
    fn and(self, o: Self) -> Self;

    /// `self | o`.
    fn or(self, o: Self) -> Self;

    /// `self | o`.
    fn or_u64(self, o: u64) -> Self;

    /// `self ^ o`.
    fn xor(self, o: Self) -> Self;

    /// `self ^ o`.
    fn xor_u64(self, o: u64) -> Self;

    /// `self * 2^s` (`mpz_mul_2exp`), where `bit_len(self) + s <= MAX_BITS`
    /// (`nat::check_result_bits`; LB-12 is lifted, so `s` may be 2^32 or
    /// more).
    fn shl(self, s: u64) -> Self;

    /// `self / 2^s` rounded down (`mpz_tdiv_q_2exp`), for any `s` (LB-04 is
    /// lifted: the rule passes shift amounts of 2^32 and more).
    fn shr(self, s: u64) -> Self;

    /// `self ^ e` (`mpz_pow_ui`), where `self >= 2`, `e >= 1` and
    /// `bit_len(self) * e <= MAX_BITS < 2^64`: the size every backend
    /// allocates for the result (GMP's `mpz_n_pow_ui` and malachite compute
    /// it in 64 bits, where a larger product wraps; review RS2-01).
    /// `nat::pow` handles bases 0 and 1 and tests the size; LB-11 is lifted,
    /// so `e` may be 2^32 or more.
    fn pow(self, e: u64) -> Self;

    /// The greatest common divisor, with `gcd 0 x = x` (`mpz_gcd`).
    fn gcd(self, o: Self) -> Self;

    /// The decimal digits, no sign, no leading zeros (`mpz_get_str` base 10).
    fn write_decimal<W: fmt::Write + ?Sized>(&self, out: &mut W) -> fmt::Result;
}

/// An integer of any size: the slow-path arithmetic of `Int`.
pub trait BigInt: Sized {
    /// The naturals of the same backend (`Int.ofNat`, `Int.natAbs`).
    type Nat: BigNat;

    /// The value `v`.
    fn from_i64(v: i64) -> Self;

    /// The value `v` (a sum, product or quotient of two `i64`s).
    fn from_i128(v: i128) -> Self;

    /// The integer of a natural number (`lean_nat_to_int`, `lean_big_int_to_nat`).
    fn from_nat(n: Self::Nat) -> Self;

    /// The absolute value (`Int.natAbs`, `lean_nat_abs`).
    fn nat_abs(self) -> Self::Nat;

    /// The value when it is in the `i64` range.
    fn to_i64(&self) -> Option<i64>;

    /// The value modulo 2^64 in two's complement (`mpz::smod64`, the
    /// `IntN.ofInt` rows).
    fn low_u64(&self) -> u64;

    /// Whether the value is below zero.
    fn is_neg(&self) -> bool;

    /// The bit length of the magnitude: 0 for zero (the size test of the
    /// `Int` rules, against `Self::Nat::MAX_BITS`).
    fn bit_len(&self) -> u64;

    /// The order of two values (`mpz_cmp`).
    fn compare(&self, o: &Self) -> Ordering;

    /// `-self`.
    fn neg(self) -> Self;

    /// `self + o`.
    fn add(self, o: Self) -> Self;

    /// `self - o`.
    fn sub(self, o: Self) -> Self;

    /// `self * o`.
    fn mul(self, o: Self) -> Self;

    /// The quotient rounded toward zero and the remainder with the sign of
    /// `self` (C's `/` and `%`, `mpz_tdiv_qr`), where `o != 0`.
    fn tdiv_rem(self, o: &Self) -> (Self, Self);

    /// `Int.tdiv`'s quotient (`mpz_tdiv_q`), where `o != 0`.
    fn tdiv(self, o: Self) -> Self {
        self.tdiv_rem(&o).0
    }

    /// `Int.tmod`'s remainder (`mpz_tdiv_r`), where `o != 0`.
    fn tmod(self, o: Self) -> Self {
        self.tdiv_rem(&o).1
    }

    /// `Int.ediv` (`mpz::ediv`, `src/runtime/mpz.cpp`): the truncated
    /// quotient, minus one when the remainder is negative and the divisor
    /// positive, plus one when both are negative; `o != 0`.
    fn ediv(self, o: Self) -> Self {
        let pos = !o.is_neg();
        let (q, r) = self.tdiv_rem(&o);
        if r.is_neg() {
            if pos {
                q.sub(Self::from_i64(1))
            } else {
                q.add(Self::from_i64(1))
            }
        } else {
            q
        }
    }

    /// `Int.emod` (`mpz::emod`): the truncated remainder, plus `|o|` when it
    /// is negative, so the result is in `[0, |o|)`; `o != 0`.
    fn emod(self, o: Self) -> Self {
        let (_, r) = self.tdiv_rem(&o);
        if r.is_neg() {
            if o.is_neg() {
                r.sub(o)
            } else {
                r.add(o)
            }
        } else {
            r
        }
    }

    /// `self / o` where `o` divides `self` and `o != 0` (`mpz_divexact`).
    fn div_exact(self, o: Self) -> Self {
        self.tdiv(o)
    }

    /// The decimal digits with a leading `-` for a negative value
    /// (`mpz_get_str` base 10).
    fn write_decimal<W: fmt::Write + ?Sized>(&self, out: &mut W) -> fmt::Result;
}

/// Backends for the rules' size tests: a value is only its bit length, so a
/// test can hold a number of 2^40 bits; every operation returns a value of
/// the bit length the rules expect of it. `MAX_BITS` is 2^40.
#[cfg(test)]
pub(crate) mod test_backend {
    use super::{BigInt, BigNat};
    use core::cmp::Ordering;
    use core::fmt;

    /// A natural number of `.0` bits.
    #[derive(Debug, PartialEq, Eq)]
    pub struct N40(pub u64);

    impl BigNat for N40 {
        const MAX_BITS: u64 = 1 << 40;
        fn from_u64(v: u64) -> N40 {
            N40(u64::from(64 - v.leading_zeros()))
        }
        fn to_u64(&self) -> Option<u64> {
            None
        }
        fn low_u64(&self) -> u64 {
            0
        }
        fn bit_len(&self) -> u64 {
            self.0
        }
        fn compare(&self, o: &N40) -> Ordering {
            self.0.cmp(&o.0)
        }
        fn add(self, o: N40) -> N40 {
            N40(self.0.max(o.0) + 1)
        }
        fn add_u64(self, _: u64) -> N40 {
            N40(self.0 + 1)
        }
        fn sub(self, _: N40) -> N40 {
            self
        }
        fn sub_u64(self, _: u64) -> N40 {
            self
        }
        fn mul(self, o: N40) -> N40 {
            N40(self.0 + o.0)
        }
        fn mul_u64(self, o: u64) -> N40 {
            N40(self.0 + u64::from(64 - o.leading_zeros()))
        }
        fn div(self, _: N40) -> N40 {
            self
        }
        fn rem(self, o: N40) -> N40 {
            o
        }
        fn div_u64(self, _: u64) -> N40 {
            self
        }
        fn rem_u64(&self, _: u64) -> u64 {
            0
        }
        fn and(self, o: N40) -> N40 {
            o
        }
        fn or(self, _: N40) -> N40 {
            self
        }
        fn or_u64(self, _: u64) -> N40 {
            self
        }
        fn xor(self, _: N40) -> N40 {
            self
        }
        fn xor_u64(self, _: u64) -> N40 {
            self
        }
        fn shl(self, s: u64) -> N40 {
            N40(self.0 + s)
        }
        fn shr(self, s: u64) -> N40 {
            N40(self.0.saturating_sub(s))
        }
        fn pow(self, e: u64) -> N40 {
            N40(self.0 * e)
        }
        fn gcd(self, o: N40) -> N40 {
            o
        }
        fn write_decimal<W: fmt::Write + ?Sized>(&self, _: &mut W) -> fmt::Result {
            Ok(())
        }
    }

    /// An integer whose magnitude has `.0` bits.
    #[derive(Debug, PartialEq, Eq)]
    pub struct I40(pub u64);

    impl BigInt for I40 {
        type Nat = N40;
        fn from_i64(v: i64) -> I40 {
            I40(u64::from(64 - v.unsigned_abs().leading_zeros()))
        }
        fn from_i128(v: i128) -> I40 {
            I40(u64::from(128 - v.unsigned_abs().leading_zeros()))
        }
        fn from_nat(n: N40) -> I40 {
            I40(n.0)
        }
        fn nat_abs(self) -> N40 {
            N40(self.0)
        }
        fn to_i64(&self) -> Option<i64> {
            None
        }
        fn low_u64(&self) -> u64 {
            0
        }
        fn is_neg(&self) -> bool {
            false
        }
        fn bit_len(&self) -> u64 {
            self.0
        }
        fn compare(&self, o: &I40) -> Ordering {
            self.0.cmp(&o.0)
        }
        fn neg(self) -> I40 {
            self
        }
        fn add(self, o: I40) -> I40 {
            I40(self.0.max(o.0) + 1)
        }
        fn sub(self, o: I40) -> I40 {
            I40(self.0.max(o.0) + 1)
        }
        fn mul(self, o: I40) -> I40 {
            I40(self.0 + o.0)
        }
        fn tdiv_rem(self, o: &I40) -> (I40, I40) {
            (self, I40(o.0))
        }
        fn write_decimal<W: fmt::Write + ?Sized>(&self, _: &mut W) -> fmt::Result {
            Ok(())
        }
    }
}
