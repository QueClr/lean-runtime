//! Lean 4.34.0's array edge rules (`include/lean/lean.h`, `src/runtime/object.cpp`):
//! out-of-bounds indices, the silent `ByteArray`/`FloatArray` accessors,
//! the size checks of the allocators, and `ByteArray.copySlice`'s ranges.
//! Each function takes lengths and indices and returns what to do; the
//! translator does it on its own array representation.
//!
//! An index or a size is a `Nat`. Here it is a `u64`: the value, or
//! `u64::MAX` for a value of 2^64 or more (`semantics::nat::Nat::
//! to_u64_saturating`). Lean's C treats every `Nat` of 2^63 or more as out
//! of bounds, which these functions do since no array has 2^63 elements.
//!
//! Rules (all from C, which differs from the Lean definitions in places):
//! - `Array.get!Internal` (`xs[i]!`) and `Array.set!` out of bounds print
//!   `Error: index out of bounds` through `lean_panic_fn` and return the
//!   default, or the array unchanged. (`Array.set!`'s Lean definition is
//!   silent; its C panics.)
//! - `Array.swapIfInBounds`, `ByteArray.set!` and `FloatArray.set!` out of
//!   bounds return the array unchanged, and `ByteArray.get!`/`FloatArray.get!`
//!   return 0/0.0, with no message. (Their Lean definitions panic; their C
//!   does not.)
//! - `Array.pop` of an empty array is the empty array.
//! - The allocators end the process with `INTERNAL PANIC: integer overflow in
//!   runtime computation` when the object size `24 + elem * n` exceeds
//!   2^64 - 1, and with `INTERNAL PANIC: out of memory` when a size argument
//!   is not a word (2^64 or more for `Array.replicate`, 2^63 or more for the
//!   capacity of `mkEmpty`/`emptyWithCapacity`) or the allocation fails.
//! - `ByteArray.copySlice` follows its Lean definition; an offset or a
//!   length of 2^64 or more is read as `u64::MAX` (LB-06 lifted: native ends
//!   with `INTERNAL PANIC: out of memory` there, `lean_nat_to_size_t`).
//!
//! Source: leanrs_rt `src/array.rs` (the silent accessors, `lean_len`'s size
//! check, `copy_slice` on clamped arguments), lean2rr's leanrt `src/array.rs`
//! (`check_alloc`, `copy_slice`) and `runtime/prelude.rr` (the replicate and
//! mkEmpty boundaries, branch fix-xt e7848d3), restated as decisions on
//! lengths.

use super::panic::InternalPanic;

/// The message `lean_array_get_panic` and `lean_array_set_panic` pass to
/// `lean_panic_fn` (`object.cpp`).
pub const INDEX_OUT_OF_BOUNDS: &str = "Error: index out of bounds";

/// `sizeof(lean_array_object)` and `sizeof(lean_sarray_object)` on 64-bit
/// targets: the header in every array object's byte size.
pub const ARRAY_HEADER_BYTES: u64 = 24;

/// The element size of an `Array` (a pointer) and of a `FloatArray` (a
/// `double`) in Lean's objects.
pub const WORD_ELEMENT_BYTES: u64 = 8;

/// The element size of a `ByteArray`.
pub const BYTE_ELEMENT_BYTES: u64 = 1;

/// An out-of-bounds `get!`/`set!`: print `INDEX_OUT_OF_BOUNDS` through the
/// translator's `lean_panic_fn`, then return the default (`get!`) or the
/// array unchanged (`set!`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfBounds;

impl OutOfBounds {
    /// The message to print.
    pub const fn message(self) -> &'static str {
        INDEX_OUT_OF_BOUNDS
    }
}

#[inline]
fn in_bounds(len: usize, i: u64) -> Option<usize> {
    if i < len as u64 {
        Some(i as usize)
    } else {
        None
    }
}

/// `Array.get!Internal` (`lean_array_get`, `xs[i]!`): the index of the
/// element, or `OutOfBounds`.
///
/// Source: leanrs_rt `src/array.rs` (`get_bang`), on a length.
#[inline]
pub fn get_bang(len: usize, i: u64) -> Result<usize, OutOfBounds> {
    in_bounds(len, i).ok_or(OutOfBounds)
}

/// `Array.set!` (`lean_array_set`): the index to store at, or `OutOfBounds`
/// (the array is returned unchanged, the value dropped).
///
/// Source: leanrs_rt `src/array.rs` (`set_bang`), on a length.
#[inline]
pub fn set_bang(len: usize, i: u64) -> Result<usize, OutOfBounds> {
    in_bounds(len, i).ok_or(OutOfBounds)
}

/// `Array.swapIfInBounds` (`lean_array_swap`): the two indices to swap, or
/// `None` when either is out of bounds (the array unchanged, no message).
///
/// Source: leanrs_rt `src/array.rs` (`swap_if_in_bounds`), on a length.
#[inline]
pub fn swap_if_in_bounds(len: usize, i: u64, j: u64) -> Option<(usize, usize)> {
    Some((in_bounds(len, i)?, in_bounds(len, j)?))
}

/// `Array.pop` (`lean_array_pop`): the new length, or `None` for an empty
/// array, which is returned as it is.
///
/// Source: new, from `lean.h`.
#[inline]
pub fn pop(len: usize) -> Option<usize> {
    len.checked_sub(1)
}

/// `ByteArray.get!` (`lean_byte_array_get`): the byte, or 0 out of bounds
/// with no message.
///
/// Source: leanrs_rt `src/array.rs` (`bytearray_get_bang`), unchanged.
#[inline]
pub fn byte_array_get(a: &[u8], i: u64) -> u8 {
    match in_bounds(a.len(), i) {
        Some(i) => a[i],
        None => 0,
    }
}

/// `ByteArray.set!` (`lean_byte_array_set`): the index to store at, or
/// `None` out of bounds (the array unchanged, no message).
///
/// Source: leanrs_rt `src/array.rs` (`bytearray_set_bang`), on a length.
#[inline]
pub fn byte_array_set(len: usize, i: u64) -> Option<usize> {
    in_bounds(len, i)
}

/// `FloatArray.get!` (`lean_float_array_get`): the element, or 0.0 out of
/// bounds with no message.
///
/// Source: leanrs_rt `src/array.rs` (`floatarray_get_bang`), unchanged.
#[inline]
pub fn float_array_get(a: &[f64], i: u64) -> f64 {
    match in_bounds(a.len(), i) {
        Some(i) => a[i],
        None => 0.0,
    }
}

/// `FloatArray.set!` (`lean_float_array_set`): the index to store at, or
/// `None` out of bounds (the array unchanged, no message).
///
/// Source: leanrs_rt `src/array.rs` (`floatarray_set_bang`), on a length.
#[inline]
pub fn float_array_set(len: usize, i: u64) -> Option<usize> {
    in_bounds(len, i)
}

/// The byte size of an array object of `n` elements of `elem` bytes
/// (`lean_alloc_array`, `lean_alloc_sarray`): `24 + elem * n`, or
/// `IntegerOverflow` when it exceeds 2^64 - 1 (`lean_usize_mul_checked`,
/// `lean_usize_add_checked`), or `OutOfMemory` when it exceeds `isize::MAX`,
/// which no allocator grants (mimalloc and glibc fail above `PTRDIFF_MAX`).
/// A smaller size can still fail to allocate; the translator's allocator
/// reports that as `OutOfMemory` too.
///
/// Source: lean2rr leanrt `src/array.rs` (`check_alloc_slow`) and leanrs_rt
/// `src/array.rs` (`lean_len`), merged; the `isize::MAX` bound is new.
#[inline]
pub fn alloc_bytes(elem: u64, n: u64) -> Result<u64, InternalPanic> {
    let bytes = elem
        .checked_mul(n)
        .and_then(|b| b.checked_add(ARRAY_HEADER_BYTES))
        .ok_or(InternalPanic::IntegerOverflow)?;
    if bytes > isize::MAX as u64 {
        return Err(InternalPanic::OutOfMemory);
    }
    Ok(bytes)
}

/// `Array.replicate n v` (`lean_mk_array`): the element count, after the
/// checks of `lean_mk_array` and `lean_alloc_array`. `n` is the size when it
/// is below 2^64 (`Nat::to_u64`), else `None`, which is `OutOfMemory`
/// (`v.is_size_t()` fails). Any size below 2^64, 2^63 and up included, goes
/// through `alloc_bytes` with 8-byte elements, so 2^61 - 3 and up overflow.
///
/// The element size is Lean's, 8 for every `Array`, whatever the
/// translator's own (lean2rr's `Array Nat` included).
///
/// Source: lean2rr's `runtime/prelude.rr` (`lean_mk_array`,
/// `l2r_nat_to_size_t`, branch fix-xt e7848d3) and leanrs_rt `src/array.rs`
/// (`replicate`), merged.
#[inline]
pub fn replicate_len(n: Option<u64>) -> Result<usize, InternalPanic> {
    let n = n.ok_or(InternalPanic::OutOfMemory)?;
    alloc_bytes(WORD_ELEMENT_BYTES, n)?;
    Ok(n as usize)
}

/// `Array.mkEmpty c`, `Array.emptyWithCapacity c`
/// (`lean_mk_empty_array_with_capacity`), `ByteArray.emptyWithCapacity c`
/// (`lean_mk_empty_byte_array`) and `FloatArray.emptyWithCapacity c`
/// (`lean_mk_empty_float_array`): the capacity to reserve. A capacity that
/// is not a word, 2^63 or more (`lean_is_scalar` fails), is `OutOfMemory`;
/// a smaller one goes through `alloc_bytes` with `elem` bytes per element
/// (8 for `Array` and `FloatArray`, 1 for `ByteArray`, whose 24 + c then
/// never overflows).
///
/// leanrs keeps its own rule here (its DV17 (d): an unreservable capacity
/// is ignored).
///
/// Source: lean2rr's `runtime/prelude.rr` (`l2r_mk_empty_with_capacity`) and
/// leanrt `src/array.rs` (`with_capacity_checked`), merged.
#[inline]
pub fn empty_with_capacity(elem: u64, c: u64) -> Result<usize, InternalPanic> {
    if c >> 63 != 0 {
        return Err(InternalPanic::OutOfMemory);
    }
    alloc_bytes(elem, c)?;
    Ok(c as usize)
}

/// What `ByteArray.copySlice` does to `dest`: the bytes
/// `src[src_start .. src_start + len]` go to `dest[dest_start ..]`, and the
/// result has `new_len` bytes (`dest`'s own beyond the copied range).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopySlice {
    pub src_start: usize,
    pub len: usize,
    pub dest_start: usize,
    pub new_len: usize,
}

/// `ByteArray.copySlice src srcOff dest destOff len exact`
/// (`lean_byte_array_copy_slice`): `None` when `srcOff` is past `src`'s end
/// (`dest` is returned unchanged), otherwise the copy to make. As the Lean
/// definition: `dest[0, destOff) ++ src[srcOff, srcOff + len) ++
/// dest[destOff + min len (src.size - srcOff), dest.size)`, with `destOff`
/// clamped to `dest`'s size. `exact` only chooses the capacity of a grown
/// result (exactly `new_len`, else twice that).
///
/// Offsets and the length of 2^64 or more are passed as `u64::MAX` and give
/// the definition's result (LB-06 lifted; native ends with `INTERNAL PANIC:
/// out of memory`).
///
/// Source: lean2rr leanrt `src/array.rs` (`copy_slice`) and leanrs_rt
/// `src/array.rs` (`copy_slice`), as a plan on lengths.
#[inline]
pub fn copy_slice(
    src_len: usize,
    src_off: u64,
    dest_len: usize,
    dest_off: u64,
    len: u64,
) -> Option<CopySlice> {
    if src_off > src_len as u64 {
        return None;
    }
    let src_start = src_off as usize;
    let len = len.min((src_len - src_start) as u64) as usize;
    let dest_start = dest_off.min(dest_len as u64) as usize;
    Some(CopySlice {
        src_start,
        len,
        dest_start,
        new_len: dest_len.max(dest_start + len),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size boundaries of `tests/cases/array` (lean2rr's RtAllocBigNat).
    #[test]
    fn sizes() {
        let oom = Err(InternalPanic::OutOfMemory);
        let overflow = Err(InternalPanic::IntegerOverflow);
        assert_eq!(replicate_len(Some(3)), Ok(3));
        assert_eq!(replicate_len(Some((1 << 61) - 4)), oom);
        assert_eq!(replicate_len(Some((1 << 61) - 3)), overflow);
        assert_eq!(replicate_len(Some(u64::MAX)), overflow);
        assert_eq!(replicate_len(None), oom);
        assert_eq!(
            empty_with_capacity(WORD_ELEMENT_BYTES, (1 << 61) - 3),
            overflow
        );
        assert_eq!(
            empty_with_capacity(WORD_ELEMENT_BYTES, (1 << 63) - 1),
            overflow
        );
        assert_eq!(empty_with_capacity(WORD_ELEMENT_BYTES, 1 << 63), oom);
        assert_eq!(empty_with_capacity(BYTE_ELEMENT_BYTES, (1 << 63) - 1), oom);
        assert_eq!(alloc_bytes(8, 0), Ok(24));
    }

    /// `copySlice` plans (`tests/cases/array`, `copyslice.*`).
    #[test]
    fn copy_slices() {
        assert_eq!(copy_slice(5, 6, 3, 0, 3), None);
        let p = copy_slice(5, 2, 3, 9, 2).unwrap();
        assert_eq!((p.src_start, p.len, p.dest_start, p.new_len), (2, 2, 3, 5));
        let p = copy_slice(5, 1, 3, 0, u64::MAX).unwrap();
        assert_eq!((p.src_start, p.len, p.dest_start, p.new_len), (1, 4, 0, 4));
        let p = copy_slice(5, 5, 3, 0, 3).unwrap();
        assert_eq!((p.len, p.new_len), (0, 3));
        assert_eq!(byte_array_get(&[1, 2], u64::MAX), 0);
        assert_eq!(get_bang(3, 3), Err(OutOfBounds));
        assert_eq!(swap_if_in_bounds(3, 0, 3), None);
        assert_eq!(pop(0), None);
    }
}
