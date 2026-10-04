//! A plain arbitrary-precision backend for the tests: `RNat` and `RInt`
//! implement `BigNat` and `BigInt` with schoolbook algorithms on `Vec<u64>`
//! limbs, written for obviousness, not speed. The rows test the crate's
//! rules through it; it is checked against `u128`/`i128` arithmetic by
//! `refbig_matches_wide_arithmetic`.
//!
//! `RNat::pow2(k)` allocates its zero limbs with `vec![0; n]` (zeroed pages
//! from the system, untouched) and writes only the top limb, so the LB-04
//! rows can hold 2^(2^32) (2^26 limbs) without touching 512 MiB, as long as
//! the operations on it read only the top limbs (`shr`, `bit_len`).

use std::cmp::Ordering;
use std::fmt;

use lean_runtime::semantics::bignum::{BigInt, BigNat};

/// A natural number: little-endian limbs, no zero limb on top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RNat(Vec<u64>);

fn trim(mut v: Vec<u64>) -> RNat {
    while v.last() == Some(&0) {
        v.pop();
    }
    RNat(v)
}

fn cmp_mag(a: &[u64], b: &[u64]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

fn add_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u128;
    for (i, &x) in a.iter().enumerate() {
        let s = x as u128 + *b.get(i).unwrap_or(&0) as u128 + carry;
        out.push(s as u64);
        carry = s >> 64;
    }
    out.push(carry as u64);
    out
}

/// `a - b` for `a >= b`.
fn sub_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    assert!(cmp_mag(a, b) != Ordering::Less, "sub_mag: a < b");
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0u64;
    for (i, &x) in a.iter().enumerate() {
        let (d1, b1) = x.overflowing_sub(*b.get(i).unwrap_or(&0));
        let (d2, b2) = d1.overflowing_sub(borrow);
        out.push(d2);
        borrow = (b1 || b2) as u64;
    }
    assert_eq!(borrow, 0);
    out
}

fn mul_mag(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u128;
        for (j, &y) in b.iter().enumerate() {
            let t = out[i + j] as u128 + x as u128 * y as u128 + carry;
            out[i + j] = t as u64;
            carry = t >> 64;
        }
        out[i + b.len()] = carry as u64;
    }
    out
}

fn bit_len_mag(a: &[u64]) -> u64 {
    match a.last() {
        None => 0,
        Some(&top) => 64 * (a.len() as u64 - 1) + (64 - top.leading_zeros() as u64),
    }
}

fn bit(a: &[u64], i: u64) -> bool {
    a.get((i / 64) as usize)
        .is_some_and(|l| (l >> (i % 64)) & 1 == 1)
}

fn shl_mag(a: &[u64], s: u64) -> Vec<u64> {
    if a.is_empty() {
        return Vec::new();
    }
    let (limbs, bits) = ((s / 64) as usize, s % 64);
    let mut out = vec![0u64; limbs];
    let mut carry = 0u64;
    for &x in a {
        if bits == 0 {
            out.push(x);
        } else {
            out.push((x << bits) | carry);
            carry = x >> (64 - bits);
        }
    }
    out.push(carry);
    out
}

fn shr_mag(a: &[u64], s: u64) -> Vec<u64> {
    let limbs = s / 64;
    if limbs >= a.len() as u64 {
        return Vec::new();
    }
    let (limbs, bits) = (limbs as usize, s % 64);
    let src = &a[limbs..];
    (0..src.len())
        .map(|i| {
            if bits == 0 {
                src[i]
            } else {
                (src[i] >> bits) | (src.get(i + 1).map_or(0, |h| h << (64 - bits)))
            }
        })
        .collect()
}

/// Binary long division: the quotient and the remainder of `a / b`, `b != 0`.
fn divrem_mag(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    assert!(!b.is_empty(), "division by zero");
    if b.len() == 1 {
        let (q, r) = divrem_u64(a, b[0]);
        return (q, vec![r]);
    }
    let n = bit_len_mag(a);
    let mut q = vec![0u64; a.len()];
    let mut r: Vec<u64> = Vec::new();
    for i in (0..n).rev() {
        r = shl_mag(&r, 1);
        if bit(a, i) {
            if r.is_empty() {
                r.push(1);
            } else {
                r[0] |= 1;
            }
        }
        r = trim(r).0;
        if cmp_mag(&r, b) != Ordering::Less {
            r = trim(sub_mag(&r, b)).0;
            q[(i / 64) as usize] |= 1 << (i % 64);
        }
    }
    (q, r)
}

fn divrem_u64(a: &[u64], d: u64) -> (Vec<u64>, u64) {
    assert!(d != 0, "division by zero");
    let mut q = vec![0u64; a.len()];
    let mut r = 0u128;
    for i in (0..a.len()).rev() {
        let cur = (r << 64) | a[i] as u128;
        q[i] = (cur / d as u128) as u64;
        r = cur % d as u128;
    }
    (q, r as u64)
}

impl RNat {
    pub fn zero() -> RNat {
        RNat(Vec::new())
    }

    /// 2^k, its low limbs zeroed pages (see the module comment).
    pub fn pow2(k: u64) -> RNat {
        let n = (k / 64) as usize;
        let mut v = vec![0u64; n + 1];
        v[n] = 1 << (k % 64);
        RNat(v)
    }

    /// 2^k + a for k >= 64, writing two limbs of the zeroed block.
    pub fn pow2_plus(k: u64, a: u64) -> RNat {
        assert!(k >= 64, "pow2_plus: k < 64");
        let mut v = RNat::pow2(k);
        v.0[0] = a;
        v
    }

    pub fn from_u128(v: u128) -> RNat {
        trim(vec![v as u64, (v >> 64) as u64])
    }

    pub fn to_u128(&self) -> Option<u128> {
        match self.0.len() {
            0 => Some(0),
            1 => Some(self.0[0] as u128),
            2 => Some(self.0[0] as u128 | (self.0[1] as u128) << 64),
            _ => None,
        }
    }

    /// Decimal digits (the row files' numerals).
    pub fn from_decimal(s: &str) -> RNat {
        assert!(
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
            "not a numeral: {s}"
        );
        let mut acc = RNat::zero();
        for chunk in s.as_bytes().chunks(18) {
            let v: u64 = std::str::from_utf8(chunk).unwrap().parse().unwrap();
            acc = trim(mul_mag(&acc.0, &[10u64.pow(chunk.len() as u32)]));
            acc = trim(add_mag(&acc.0, &[v]));
        }
        acc
    }

    pub fn to_decimal(&self) -> String {
        if self.0.is_empty() {
            return "0".into();
        }
        const BASE: u64 = 10_000_000_000_000_000_000;
        let mut parts = Vec::new();
        let mut cur = self.0.clone();
        while !cur.is_empty() {
            let (q, r) = divrem_u64(&cur, BASE);
            parts.push(r);
            cur = trim(q).0;
        }
        let mut s = parts.pop().unwrap().to_string();
        for p in parts.iter().rev() {
            s.push_str(&format!("{p:019}"));
        }
        s
    }
}

impl BigNat for RNat {
    fn from_u64(v: u64) -> RNat {
        trim(vec![v])
    }
    fn to_u64(&self) -> Option<u64> {
        match self.0.len() {
            0 => Some(0),
            1 => Some(self.0[0]),
            _ => None,
        }
    }
    fn low_u64(&self) -> u64 {
        self.0.first().copied().unwrap_or(0)
    }
    fn bit_len(&self) -> u64 {
        bit_len_mag(&self.0)
    }
    fn compare(&self, o: &RNat) -> Ordering {
        cmp_mag(&self.0, &o.0)
    }
    fn add(self, o: RNat) -> RNat {
        trim(add_mag(&self.0, &o.0))
    }
    fn add_u64(self, o: u64) -> RNat {
        trim(add_mag(&self.0, &[o]))
    }
    fn sub(self, o: RNat) -> RNat {
        trim(sub_mag(&self.0, &o.0))
    }
    fn sub_u64(self, o: u64) -> RNat {
        trim(sub_mag(&self.0, &[o]))
    }
    fn mul(self, o: RNat) -> RNat {
        trim(mul_mag(&self.0, &o.0))
    }
    fn mul_u64(self, o: u64) -> RNat {
        trim(mul_mag(&self.0, &[o]))
    }
    fn div(self, o: RNat) -> RNat {
        trim(divrem_mag(&self.0, &o.0).0)
    }
    fn rem(self, o: RNat) -> RNat {
        trim(divrem_mag(&self.0, &o.0).1)
    }
    fn div_u64(self, o: u64) -> RNat {
        trim(divrem_u64(&self.0, o).0)
    }
    fn rem_u64(&self, o: u64) -> u64 {
        divrem_u64(&self.0, o).1
    }
    fn and(self, o: RNat) -> RNat {
        trim(self.0.iter().zip(&o.0).map(|(x, y)| x & y).collect())
    }
    fn or(self, o: RNat) -> RNat {
        let n = self.0.len().max(o.0.len());
        trim(
            (0..n)
                .map(|i| self.0.get(i).unwrap_or(&0) | o.0.get(i).unwrap_or(&0))
                .collect(),
        )
    }
    fn or_u64(self, o: u64) -> RNat {
        self.or(RNat::from_u64(o))
    }
    fn xor(self, o: RNat) -> RNat {
        let n = self.0.len().max(o.0.len());
        trim(
            (0..n)
                .map(|i| self.0.get(i).unwrap_or(&0) ^ o.0.get(i).unwrap_or(&0))
                .collect(),
        )
    }
    fn xor_u64(self, o: u64) -> RNat {
        self.xor(RNat::from_u64(o))
    }
    fn shl(self, s: u64) -> RNat {
        trim(shl_mag(&self.0, s))
    }
    fn shr(self, s: u64) -> RNat {
        trim(shr_mag(&self.0, s))
    }
    fn pow(self, mut e: u32) -> RNat {
        let mut base = self;
        let mut acc = RNat::from_u64(1);
        while e > 0 {
            if e & 1 == 1 {
                acc = acc.mul(base.clone());
            }
            e >>= 1;
            if e > 0 {
                base = base.clone().mul(base);
            }
        }
        acc
    }
    fn gcd(self, o: RNat) -> RNat {
        let (mut a, mut b) = (self, o);
        while !b.0.is_empty() {
            let r = a.rem(b.clone());
            a = b;
            b = r;
        }
        a
    }
    fn write_decimal<W: fmt::Write + ?Sized>(&self, out: &mut W) -> fmt::Result {
        out.write_str(&self.to_decimal())
    }
}

/// An integer: a sign and a magnitude, zero never negative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RInt {
    neg: bool,
    mag: RNat,
}

impl RInt {
    fn new(neg: bool, mag: RNat) -> RInt {
        RInt {
            neg: neg && !mag.0.is_empty(),
            mag,
        }
    }

    pub fn to_decimal(&self) -> String {
        format!(
            "{}{}",
            if self.neg { "-" } else { "" },
            self.mag.to_decimal()
        )
    }

    pub fn to_i128(&self) -> Option<i128> {
        let m = i128::try_from(self.mag.to_u128()?).ok()?;
        Some(if self.neg { -m } else { m })
    }
}

impl BigInt for RInt {
    type Nat = RNat;

    fn from_i64(v: i64) -> RInt {
        RInt::new(v < 0, RNat::from_u64(v.unsigned_abs()))
    }
    fn from_i128(v: i128) -> RInt {
        RInt::new(v < 0, RNat::from_u128(v.unsigned_abs()))
    }
    fn from_nat(n: RNat) -> RInt {
        RInt::new(false, n)
    }
    fn nat_abs(self) -> RNat {
        self.mag
    }
    fn to_i64(&self) -> Option<i64> {
        i64::try_from(self.to_i128()?).ok()
    }
    fn low_u64(&self) -> u64 {
        let m = self.mag.low_u64();
        if self.neg {
            m.wrapping_neg()
        } else {
            m
        }
    }
    fn is_neg(&self) -> bool {
        self.neg
    }
    fn compare(&self, o: &RInt) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.mag.compare(&o.mag),
            (true, true) => o.mag.compare(&self.mag),
        }
    }
    fn neg(self) -> RInt {
        RInt::new(!self.neg, self.mag)
    }
    fn add(self, o: RInt) -> RInt {
        if self.neg == o.neg {
            RInt::new(self.neg, self.mag.add(o.mag))
        } else if self.mag.compare(&o.mag) != Ordering::Less {
            RInt::new(self.neg, self.mag.sub(o.mag))
        } else {
            RInt::new(o.neg, o.mag.sub(self.mag))
        }
    }
    fn sub(self, o: RInt) -> RInt {
        self.add(o.neg())
    }
    fn mul(self, o: RInt) -> RInt {
        RInt::new(self.neg != o.neg, self.mag.mul(o.mag))
    }
    fn tdiv_rem(self, o: &RInt) -> (RInt, RInt) {
        let (q, r) = divrem_mag(&self.mag.0, &o.mag.0);
        (
            RInt::new(self.neg != o.neg, trim(q)),
            RInt::new(self.neg, trim(r)),
        )
    }
    fn write_decimal<W: fmt::Write + ?Sized>(&self, out: &mut W) -> fmt::Result {
        out.write_str(&self.to_decimal())
    }
}
