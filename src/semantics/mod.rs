//! Lean's runtime semantics on views and plain data: hashing, float
//! formatting and conversions, the libm rows, the fixed-width integer rows,
//! the string-position algorithms on UTF-8 bytes, the `Nat`/`Int` rules over
//! the big-number traits each translator implements (`bignum`), array edge
//! rules, panic texts and exit statuses, and the text of leaf values. Nothing
//! here owns or allocates a Lean value; text goes into a caller-supplied
//! `fmt::Write`.
//!
//! Each public function names the Lean 4.34.0 C function (or the Lean
//! definition a translator replaces) it mirrors and the
//! implementation it came from (leanrs_rt, lean2rr's leanrt, or new), and is
//! checked against native Lean 4.34.0 by the rows in
//! `tests/cases/<area>/<area>.rows.toml`.
//!
//! The functions assume a 64-bit platform, as both translators do: `USize`
//! and `ISize` are 64 bits, and positions, indices and sizes are `u64`s that
//! index with `as usize`.
//!
//! Platform pin: the expected values are native Lean 4.34.0 on aarch64 Linux
//! with glibc 2.39, the host both translators run on. `libm` calls the
//! platform's libm, so it matches native Lean wherever that libm is the same
//! glibc; its ports of glibc's `cbrt`, `cbrtf`, `atanh` and `atanhf` reproduce
//! glibc 2.39's aarch64 build and are defined only on aarch64 Linux with glibc (target_env = "gnu"). On
//! another target, code that calls them fails to compile until a port for
//! that platform is added and checked against its native Lean.

pub mod array;
pub mod bignum;
pub mod float;
pub mod float32;
pub mod hash;
pub mod int;
pub mod libm;
pub mod nat;
pub mod panic;
pub mod repr;
pub mod sint;
pub mod string;
pub mod uint;

const _: () = assert!(
    usize::BITS == 64,
    "lean-runtime's semantics assume a 64-bit platform"
);
