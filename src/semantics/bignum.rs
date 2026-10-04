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
//! 2026-10-03: LB-04 and LB-06 compute the definition's result, and the LB-05
//! requirement to reproduce GMP's limb cap is withdrawn). A backend still
//! stops where its library stops:
//! - **GMP.** `mpz_t` sizes are `int`s: an `mpz_*` function whose allocation
//!   request exceeds `INT_MAX` limbs (2^31 - 1 limbs, 16 GiB) calls
//!   `__gmp_overflow_in_mpz`. In GMP 6.3.0, the version Lean 4.34.0 links
//!   (`lib/libgmp.a` of the toolchain), that raises `SIGFPE` with no message
//!   (`errno.c`: `__gmp_exception` raises `SIGFPE`, then `abort`s), so the
//!   process dies with status 136 (128 + 8); native Lean behaves the same,
//!   e.g. for `(2^62)^(2^32 - 1)`, whose `mpz_n_pow_ui` asks for about 2^32
//!   limbs at once. A GMP-backed implementation reaches this limit in the
//!   operations it routes through `mpz` functions on an `mpz_t`: lean2rr's
//!   `big.rs` does so for `pow` (`mpz_pow_ui`), `gcd` (`mpz_gcd`) and the
//!   decimal conversions (`mpz_get_str`, `mpz_set_str`); its `add`, `sub`,
//!   `mul`, divisions, shifts and bitwise operations call `mpn` functions
//!   on its own blocks, whose limit is its own block size (`MAX_LIMBS`,
//!   `i32::MAX` limbs, then `INTERNAL PANIC: out of memory`).
//! - **malachite** has no limit below the address space; an allocation
//!   failure is Rust's allocation-failure abort.
//!
//! Source: new (decision Q4). The method set is what lean2rr's leanrt
//! `src/big.rs` and leanrs_rt `src/nat.rs`/`src/int.rs` already compute
//! (`nat_add_u64`, `nat_mod_u64`, `int_tdiv`, `significant_bits`, ...), so
//! each implementation is a thin layer over existing code.

use core::cmp::Ordering;
use core::fmt;

/// A natural number of any size: the slow-path arithmetic of `Nat`.
pub trait BigNat: Sized {
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

    /// `self + o`.
    fn add(self, o: Self) -> Self;

    /// `self + o`.
    fn add_u64(self, o: u64) -> Self;

    /// `self - o`, where `self >= o` (the rule truncates first).
    fn sub(self, o: Self) -> Self;

    /// `self - o`, where `self >= o`.
    fn sub_u64(self, o: u64) -> Self;

    /// `self * o`.
    fn mul(self, o: Self) -> Self;

    /// `self * o`.
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

    /// `self * 2^s` (`mpz_mul_2exp`). The rules call it with `s < 2^32`
    /// (`Nat.shiftLeft`'s limit, kept as native).
    fn shl(self, s: u64) -> Self;

    /// `self / 2^s` rounded down (`mpz_tdiv_q_2exp`), for any `s` (LB-04 is
    /// lifted: the rule passes shift amounts of 2^32 and more).
    fn shr(self, s: u64) -> Self;

    /// `self ^ e` (`mpz_pow_ui`), with `0 ^ 0 = 1`.
    fn pow(self, e: u32) -> Self;

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
