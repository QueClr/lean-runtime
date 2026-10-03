//! The `UInt8`/`UInt16`/`UInt32`/`UInt64`/`USize` externs of Lean 4.34.0 whose
//! C definition (`include/lean/lean.h`) is not a single Rust operator:
//! division and remainder by zero, shifts by the amount modulo the width,
//! `log2`, and the `Nat` conversions. Every other row (wrapping `+ - *`,
//! negation, the bitwise operations, comparisons, the casts between widths) is
//! the native Rust operation.
//!
//! `Nat` arguments are passed as the `Nat` modulo 2^64 (for a small `Nat`, its
//! value; for a big one, its low 64 bits, which is what
//! `lean_uintN_of_big_nat` computes with `mpz::mod64` and friends), so the
//! caller keeps its own `Nat` representation.
//!
//! Source: leanrs_rt `src/uint.rs` (adapted: `of_nat` takes the low 64 bits
//! instead of leanrs's `&Nat`; `to_nat` added). lean2rr generates the same
//! rows as Reussir code (`runtime/gen_scalars.py`).

macro_rules! uint_rows {
    ($t:ty, $c:literal, $div:ident, $rem:ident, $shl:ident, $shr:ident, $log2:ident,
     $of_nat:ident, $to_nat:ident) => {
        #[doc = concat!("`", $c, ".div` (`lean_", stringify!($div), "`, `lean.h`): `x / 0 = 0`.")]
        ///
        /// Source: leanrs_rt `src/uint.rs`, unchanged.
        #[inline]
        pub fn $div(a: $t, b: $t) -> $t {
            if b == 0 {
                0
            } else {
                a / b
            }
        }

        #[doc = concat!("`", $c, ".mod` (`lean_", stringify!($rem), "`, `lean.h`): `x % 0 = x`.")]
        ///
        /// Source: leanrs_rt `src/uint.rs`, unchanged.
        #[inline]
        pub fn $rem(a: $t, b: $t) -> $t {
            if b == 0 {
                a
            } else {
                a % b
            }
        }

        #[doc = concat!("`", $c, ".shiftLeft` (`lean_", stringify!($shl),
                    "`, `lean.h`): shift left by `b % ", stringify!($t), "::BITS`.")]
        ///
        /// Source: leanrs_rt `src/uint.rs`, unchanged (`wrapping_shl` masks
        /// the amount to the same low bits as `b % BITS`).
        #[inline]
        pub fn $shl(a: $t, b: $t) -> $t {
            a.wrapping_shl((b % (<$t>::BITS as $t)) as u32)
        }

        #[doc = concat!("`", $c, ".shiftRight` (`lean_", stringify!($shr),
                    "`, `lean.h`): logical shift right by `b % ", stringify!($t), "::BITS`.")]
        ///
        /// Source: leanrs_rt `src/uint.rs`, unchanged.
        #[inline]
        pub fn $shr(a: $t, b: $t) -> $t {
            a.wrapping_shr((b % (<$t>::BITS as $t)) as u32)
        }

        #[doc = concat!("`", $c, ".log2` (`lean_", stringify!($log2),
                    "`, `lean.h`): the floor of the base-2 logarithm, `log2 0 = 0`. The C ",
                    "loop halves the value; `ilog2` is the same number in one instruction.")]
        ///
        /// Source: leanrs_rt `src/uint.rs`, unchanged.
        #[inline]
        pub fn $log2(a: $t) -> $t {
            match a.checked_ilog2() {
                Some(l) => l as $t,
                None => 0,
            }
        }

        #[doc = concat!("`", $c, ".ofNat` and `Nat.to", $c, "` (`lean_", stringify!($of_nat),
                    "`, `lean.h`): the `Nat` modulo the type's size, from the `Nat` modulo 2^64.")]
        ///
        /// Source: leanrs_rt `src/uint.rs`, adapted to take the low 64 bits.
        #[inline]
        pub fn $of_nat(n_low64: u64) -> $t {
            n_low64 as $t
        }

        #[doc = concat!("`", $c, ".toNat` (`lean_", stringify!($to_nat),
                    "`, `lean.h`): the value, zero-extended; the caller makes its `Nat`.")]
        ///
        /// Source: new (a widening cast).
        #[inline]
        pub fn $to_nat(a: $t) -> u64 {
            a as u64
        }
    };
}

uint_rows!(
    u8,
    "UInt8",
    uint8_div,
    uint8_mod,
    uint8_shift_left,
    uint8_shift_right,
    uint8_log2,
    uint8_of_nat,
    uint8_to_nat
);
uint_rows!(
    u16,
    "UInt16",
    uint16_div,
    uint16_mod,
    uint16_shift_left,
    uint16_shift_right,
    uint16_log2,
    uint16_of_nat,
    uint16_to_nat
);
uint_rows!(
    u32,
    "UInt32",
    uint32_div,
    uint32_mod,
    uint32_shift_left,
    uint32_shift_right,
    uint32_log2,
    uint32_of_nat,
    uint32_to_nat
);
uint_rows!(
    u64,
    "UInt64",
    uint64_div,
    uint64_mod,
    uint64_shift_left,
    uint64_shift_right,
    uint64_log2,
    uint64_of_nat,
    uint64_to_nat
);
uint_rows!(
    usize,
    "USize",
    usize_div,
    usize_mod,
    usize_shift_left,
    usize_shift_right,
    usize_log2,
    usize_of_nat,
    usize_to_nat
);
