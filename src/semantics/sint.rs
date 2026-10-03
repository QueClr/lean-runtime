//! The `Int8`/`Int16`/`Int32`/`Int64`/`ISize` externs of Lean 4.34.0 whose C
//! definition (`include/lean/lean.h`) depends on the sign or is not a single
//! Rust operator.
//!
//! Lean's C passes each signed type as the unsigned type of its width
//! (`uint8_t` for `Int8`, ...) and casts to the signed type inside each
//! primitive whose result depends on the sign. Both translators hold the same
//! two's-complement encoding, so these functions take and return it (`u8` ..
//! `u64`, `usize` for `ISize`) and read it as `iN` exactly where `lean.h`
//! does. The rows whose bits do not depend on the sign (wrapping `+ - *`,
//! negation, complement, the bitwise operations, `==`) are the unsigned
//! operations.
//!
//! `Int` and `Nat` arguments are passed as their value modulo 2^64 in two's
//! complement (for a small value, the value; for a big one, what
//! `mpz::smod64` and friends compute), and `Int` results are returned as
//! `i64`; the caller keeps its own `Int` and `Nat` representations.
//!
//! Source: leanrs_rt `src/sint.rs` (adapted: `to_int` returns `i64` instead of
//! leanrs's `Int`; `of_int`, `of_nat`, `shift_left` and the narrowing
//! conversions added from `lean.h`). lean2rr generates the same rows as
//! Reussir code (`runtime/gen_scalars.py`).

macro_rules! sint_rows {
    ($u:ty, $s:ty, $c:literal, $div:ident, $rem:ident, $shl:ident, $shr:ident, $abs:ident,
     $lt:ident, $le:ident, $to_int:ident, $of_int:ident, $of_nat:ident, $to_float:ident,
     $to_float32:ident) => {
        #[doc = concat!("`", $c, ".div` (`lean_", stringify!($div), "`, `lean.h`): truncation ",
            "toward zero; `x / 0 = 0`; `MIN / -1 = MIN` (C widens or tests to avoid the trap).")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $div(a: $u, b: $u) -> $u {
            if b == 0 {
                0
            } else {
                (a as $s).wrapping_div(b as $s) as $u
            }
        }

        #[doc = concat!("`", $c, ".mod` (`lean_", stringify!($rem), "`, `lean.h`): the C ",
            "remainder, with the dividend's sign; `x % 0 = x`; `MIN % -1 = 0`.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $rem(a: $u, b: $u) -> $u {
            if b == 0 {
                a
            } else {
                (a as $s).wrapping_rem(b as $s) as $u
            }
        }

        #[doc = concat!("`", $c, ".shiftLeft` (`lean_", stringify!($shl), "`, `lean.h`): ",
            "shift the encoding left by `b smod BITS`, which for a power-of-two width is ",
            "`b % BITS` on the encoding.")]
        ///
        /// Source: new, from `lean.h` (leanrs_rt uses its `uint` shift).
        #[inline]
        pub fn $shl(a: $u, b: $u) -> $u {
            a.wrapping_shl((b % (<$u>::BITS as $u)) as u32)
        }

        #[doc = concat!("`", $c, ".shiftRight` (`lean_", stringify!($shr), "`, `lean.h`): ",
            "arithmetic shift right by `b smod BITS` (`b % BITS` on the encoding).")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $shr(a: $u, b: $u) -> $u {
            (a as $s).wrapping_shr((b % (<$u>::BITS as $u)) as u32) as $u
        }

        #[doc = concat!("`", $c, ".abs` (`lean_", stringify!($abs), "`, `lean.h`): ",
            "`abs MIN = MIN`.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $abs(a: $u) -> $u {
            (a as $s).wrapping_abs() as $u
        }

        #[doc = concat!("`", $c, ".decLt` (`lean_", stringify!($lt), "`, `lean.h`): ",
            "the signed order.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $lt(a: $u, b: $u) -> bool {
            (a as $s) < (b as $s)
        }

        #[doc = concat!("`", $c, ".decLe` (`lean_", stringify!($le), "`, `lean.h`): ",
            "the signed order.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $le(a: $u, b: $u) -> bool {
            (a as $s) <= (b as $s)
        }

        #[doc = concat!("`", $c, ".toInt` (`lean_", stringify!($to_int), "`, `lean.h`): ",
            "the signed value, sign-extended; the caller makes its `Int`.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, adapted to return `i64`.
        #[inline]
        pub fn $to_int(a: $u) -> i64 {
            (a as $s) as i64
        }

        #[doc = concat!("`", $c, ".ofInt` (`lean_", stringify!($of_int), "`, `lean.h`): ",
            "the `Int` modulo the type's size, from the `Int` modulo 2^64 in two's complement.")]
        ///
        /// Source: new, from `lean.h` (a truncating cast).
        #[inline]
        pub fn $of_int(i_low64: u64) -> $u {
            i_low64 as $u
        }

        #[doc = concat!("`", $c, ".ofNat` (`lean_", stringify!($of_nat), "`, `lean.h`): ",
            "the `Nat` modulo the type's size, from the `Nat` modulo 2^64.")]
        ///
        /// Source: new, from `lean.h` (a truncating cast).
        #[inline]
        pub fn $of_nat(n_low64: u64) -> $u {
            n_low64 as $u
        }

        #[doc = concat!("`", $c, ".toFloat` (`lean_", stringify!($to_float), "`, `lean.h`): ",
            "the signed value, rounded to nearest (ties to even), as C's conversion.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $to_float(a: $u) -> f64 {
            (a as $s) as f64
        }

        #[doc = concat!("`", $c, ".toFloat32` (`lean_", stringify!($to_float32), "`, `lean.h`): ",
            "the signed value, rounded to nearest (ties to even), as C's conversion.")]
        ///
        /// Source: leanrs_rt `src/sint.rs`, unchanged.
        #[inline]
        pub fn $to_float32(a: $u) -> f32 {
            (a as $s) as f32
        }
    };
}

sint_rows!(
    u8,
    i8,
    "Int8",
    int8_div,
    int8_mod,
    int8_shift_left,
    int8_shift_right,
    int8_abs,
    int8_dec_lt,
    int8_dec_le,
    int8_to_int,
    int8_of_int,
    int8_of_nat,
    int8_to_float,
    int8_to_float32
);
sint_rows!(
    u16,
    i16,
    "Int16",
    int16_div,
    int16_mod,
    int16_shift_left,
    int16_shift_right,
    int16_abs,
    int16_dec_lt,
    int16_dec_le,
    int16_to_int,
    int16_of_int,
    int16_of_nat,
    int16_to_float,
    int16_to_float32
);
sint_rows!(
    u32,
    i32,
    "Int32",
    int32_div,
    int32_mod,
    int32_shift_left,
    int32_shift_right,
    int32_abs,
    int32_dec_lt,
    int32_dec_le,
    int32_to_int,
    int32_of_int,
    int32_of_nat,
    int32_to_float,
    int32_to_float32
);
sint_rows!(
    u64,
    i64,
    "Int64",
    int64_div,
    int64_mod,
    int64_shift_left,
    int64_shift_right,
    int64_abs,
    int64_dec_lt,
    int64_dec_le,
    int64_to_int_sint,
    int64_of_int,
    int64_of_nat,
    int64_to_float,
    int64_to_float32
);
sint_rows!(
    usize,
    isize,
    "ISize",
    isize_div,
    isize_mod,
    isize_shift_left,
    isize_shift_right,
    isize_abs,
    isize_dec_lt,
    isize_dec_le,
    isize_to_int,
    isize_of_int,
    isize_of_nat,
    isize_to_float,
    isize_to_float32
);

macro_rules! convert {
    ($($name:ident: $u:ty as $s:ty => $ts:ty as $tu:ty;)*) => {
        $(
            #[doc = concat!("`lean_", stringify!($name), "` (`lean.h`): read the encoding as `",
                stringify!($s), "`, sign-extend or truncate to `", stringify!($ts),
                "`, return its encoding.")]
            ///
            /// Source: leanrs_rt `src/sint.rs` (`widen!`), extended to the
            /// narrowing conversions.
            #[inline]
            pub fn $name(a: $u) -> $tu {
                ((a as $s) as $ts) as $tu
            }
        )*
    };
}

convert! {
    int8_to_int16: u8 as i8 => i16 as u16;
    int8_to_int32: u8 as i8 => i32 as u32;
    int8_to_int64: u8 as i8 => i64 as u64;
    int8_to_isize: u8 as i8 => isize as usize;
    int16_to_int8: u16 as i16 => i8 as u8;
    int16_to_int32: u16 as i16 => i32 as u32;
    int16_to_int64: u16 as i16 => i64 as u64;
    int16_to_isize: u16 as i16 => isize as usize;
    int32_to_int8: u32 as i32 => i8 as u8;
    int32_to_int16: u32 as i32 => i16 as u16;
    int32_to_int64: u32 as i32 => i64 as u64;
    int32_to_isize: u32 as i32 => isize as usize;
    int64_to_int8: u64 as i64 => i8 as u8;
    int64_to_int16: u64 as i64 => i16 as u16;
    int64_to_int32: u64 as i64 => i32 as u32;
    int64_to_isize: u64 as i64 => isize as usize;
    isize_to_int8: usize as isize => i8 as u8;
    isize_to_int16: usize as isize => i16 as u16;
    isize_to_int32: usize as isize => i32 as u32;
    isize_to_int64: usize as isize => i64 as u64;
}
