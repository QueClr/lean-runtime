//! Lean's runtime semantics on views and plain data: hashing, float
//! formatting and conversions, the libm rows, the fixed-width integer rows and
//! the string-position algorithms on UTF-8 bytes. Nothing here owns or
//! allocates a Lean value; text goes into a caller-supplied `fmt::Write`.
//!
//! Each public function names the Lean 4.34.0 C function it mirrors and the
//! implementation it came from (leanrs_rt, lean2rr's leanrt, or new), and is
//! checked against native Lean 4.34.0 by the rows in `tests/cases/*.rows`.
//!
//! The functions assume a 64-bit platform, as both translators do: `USize`
//! and `ISize` are 64 bits, and positions are `u64`s that index with `as
//! usize`.

pub mod float;
pub mod float32;
pub mod hash;

const _: () = assert!(
    usize::BITS == 64,
    "lean-runtime's semantics assume a 64-bit platform"
);
