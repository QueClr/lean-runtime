//! Lean's runtime semantics on views and plain data: hashing, float
//! formatting and conversions, the libm rows, the fixed-width integer rows,
//! the string-position algorithms on UTF-8 bytes (with `String.Pos.Raw.set`
//! and `ByteArray.validateUTF8`), the `Nat`/`Int` rules over the big-number
//! traits each translator implements (`bignum`), array edge rules, panic
//! texts and exit statuses, the text of leaf values, the text forms of IP
//! addresses (`net`) and the toolchain's build facts (`toolchain`). Nothing
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
//! glibc. Its ports `cbrt` and `cbrtf` give glibc 2.39's aarch64 results on
//! every target, and `atanh` and `atanhf` are glibc's formula over the
//! platform's `log1p`; off aarch64 Linux the four can differ from that
//! platform's native Lean (`libm`'s module doc).
//! `toolchain::PLATFORM_TARGET` is the build target's triple.

pub mod array;
pub mod bignum;
pub mod float;
pub mod float32;
pub mod hash;
pub mod int;
pub mod libm;
pub mod nat;
pub mod net;
pub mod panic;
pub mod repr;
pub mod sint;
pub mod string;
pub mod toolchain;
pub mod uint;

const _: () = assert!(
    usize::BITS == 64,
    "lean-runtime's semantics assume a 64-bit platform"
);
