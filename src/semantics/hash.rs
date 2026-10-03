//! Lean's hashes, bit for bit: `mixHash` and MurmurHash64A (Lean 4.34.0
//! `src/runtime/hash.{h,cpp}`), behind `String.hash`, `ByteArray.hash` and
//! `String.Slice.hash`. They decide the iteration order of `Std.HashMap`,
//! which programs can observe.
//!
//! Source: leanrs_rt `src/hash.rs`, adapted (one entry point per C function,
//! and the seeded `hash_str` public under its C name). lean2rr's leanrt
//! `src/hash.rs` is the same algorithm; both agree with native Lean 4.34.0 on
//! the rows in `tests/cases/hash/hash.rows.toml`.

/// The multiplier and shift shared by both hashes (`hash.h`, `hash.cpp`).
const M: u64 = 0xc6a4_a793_5bd1_e995;
const R: u32 = 47;

/// The seed `lean_string_hash`, `lean_byte_array_hash` and `lean_slice_hash`
/// pass to `hash_str`.
const SEED: u64 = 11;

/// `mixHash` (`lean_uint64_mix_hash`, `include/lean/lean.h`, and the same
/// `lean::hash(uint64, uint64)` in `src/runtime/hash.h`). The third step is
/// `k ^= m`, not a multiplication, exactly as in Lean.
///
/// Source: leanrs_rt `src/hash.rs` (`mix_hash`), unchanged.
#[inline]
pub fn uint64_mix_hash(h: u64, k: u64) -> u64 {
    let mut k = k.wrapping_mul(M);
    k ^= k >> R;
    k ^= M;
    (h ^ k).wrapping_mul(M)
}

/// MurmurHash64A of `data` with `seed` (`lean::hash_str`, which calls
/// `MurmurHash64A`, `src/runtime/hash.cpp`): 8-byte little-endian blocks, the
/// 1 to 7 tail bytes xored in at `8 * i` bits and one multiplication, then the
/// final avalanche; all arithmetic wrapping.
///
/// Source: leanrs_rt `src/hash.rs` (`murmur64a_seeded`), made public. The
/// blocks are read with `as_chunks`, so the loop has no bounds checks, as
/// the C `memcpy` loop.
#[inline]
pub fn hash_str(data: &[u8], seed: u64) -> u64 {
    let mut h = seed ^ (data.len() as u64).wrapping_mul(M);
    let (blocks, tail) = data.as_chunks::<8>();
    for block in blocks {
        let mut k = u64::from_le_bytes(*block);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h ^= k;
        h = h.wrapping_mul(M);
    }
    if !tail.is_empty() {
        for (i, &b) in tail.iter().enumerate() {
            h ^= u64::from(b) << (8 * i);
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> R;
    h = h.wrapping_mul(M);
    h ^= h >> R;
    h
}

/// `String.hash` (`lean_string_hash`, `src/runtime/object.cpp`): `hash_str` of
/// the string's bytes (without the C terminator) with seed 11.
///
/// Source: leanrs_rt `src/hash.rs` (`murmur64a`), renamed.
#[inline]
pub fn string_hash(s: &[u8]) -> u64 {
    hash_str(s, SEED)
}

/// `ByteArray.hash` (`lean_byte_array_hash`, `src/runtime/object.cpp`):
/// `hash_str` of the bytes with seed 11, the same function as `string_hash`.
///
/// Source: leanrs_rt `src/hash.rs` (`murmur64a`), renamed.
#[inline]
pub fn byte_array_hash(a: &[u8]) -> u64 {
    hash_str(a, SEED)
}

/// `String.Slice.hash` (`lean_slice_hash`, `src/runtime/object.cpp`):
/// `hash_str` with seed 11 of the slice's bytes `[start, end)`, so a slice
/// hashes as its copy does.
///
/// Source: leanrs_rt `src/str.rs` (`slice_hash`), adapted to a byte view.
#[inline]
pub fn slice_hash(bytes: &[u8]) -> u64 {
    hash_str(bytes, SEED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty input is only the seed through the avalanche.
    #[test]
    fn empty_is_the_avalanche_of_the_seed() {
        let mut h: u64 = 11;
        h ^= h >> 47;
        h = h.wrapping_mul(M);
        h ^= h >> 47;
        assert_eq!(string_hash(b""), h);
    }
}
