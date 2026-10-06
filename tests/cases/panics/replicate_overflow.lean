-- Array allocators asked for a huge size. Natively the message depends on
-- the allocator and on the size (lean.h, object.cpp):
-- - `Array.replicate n` (`lean_mk_array`) takes any `n` below 2^64 as a
--   `size_t`, also a big `Nat` (2^63 or more), and `lean_alloc_array`'s byte
--   size `24 + 8n` then overflows: `integer overflow in runtime
--   computation`; 2^64 or more is `out of memory`;
-- - `Array.mkEmpty`, `ByteArray.emptyWithCapacity` and
--   `FloatArray.emptyWithCapacity` are `out of memory` for every big `Nat`;
--   a small capacity overflows like `replicate` (`24 + elem * n`) or fails
--   to allocate (`out of memory`).
-- The case expects the empty array for those capacities (LB-37, docs/lean-bugs.md).
-- The .pipe runs one allocation per process; the allocator and the size come
-- from the command line. (lean2rr's `replicate` said `out of memory` for
-- every big `Nat`: cross-test XT-5, leanrs prims `panics` row
-- `array_replicate_nonscalar`.) `Nat` and `Int` elements are separate cases
-- because translators may store them unboxed.

def main (args : List String) : IO Unit := do
  let n := args[1]!.toNat!
  match args[0]! with
  | "replicate" => IO.println (Array.replicate n "s").size
  | "replicateNat" => IO.println (Array.replicate n (7 : Nat)).size
  | "replicateInt" => IO.println (Array.replicate n (-7 : Int)).size
  | "replicateFloat" => IO.println (Array.replicate n (1.5 : Float)).size
  | "mkEmpty" => IO.println (Array.mkEmpty (α := String) n).size
  | "mkEmptyNat" => IO.println (Array.mkEmpty (α := Nat) n).size
  | "byteArray" => IO.println (ByteArray.emptyWithCapacity n).size
  | "floatArray" => IO.println (FloatArray.emptyWithCapacity n).size
  | _ => IO.println "unknown case"
